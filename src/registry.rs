use std::{collections::BTreeSet, path::Path};

use serde::{Deserialize, Serialize};

use crate::protocol::{ClspError, ErrorCode};

const BUILTIN: &str = include_str!("../registry/servers.toml");
const APPROVED_IDS: [&str; 7] = [
    "astro",
    "clangd",
    "gopls",
    "pyright",
    "rust",
    "typescript",
    "yaml-ls",
];
const APPROVED_RUNTIME_IDS: [&str; 2] = ["node", "npm-cli"];
const APPROVED_EXTENSIONS: [&str; 25] = [
    "astro", "c", "c++", "cc", "cjs", "cpp", "cts", "cxx", "go", "h", "h++", "hh", "hpp", "hxx",
    "js", "jsx", "mjs", "mts", "py", "pyi", "rs", "ts", "tsx", "yaml", "yml",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub server: Vec<ServerDefinition>,
    pub runtime: Vec<RuntimeDefinition>,
}

impl Registry {
    pub fn builtin() -> Result<Self, ClspError> {
        let registry: Self = toml::from_str(BUILTIN).map_err(registry_error)?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn validate(&self) -> Result<(), ClspError> {
        let expected: BTreeSet<_> = APPROVED_IDS.into_iter().collect();
        let actual: BTreeSet<_> = self
            .server
            .iter()
            .map(|server| server.id.as_str())
            .collect();
        if actual != expected || actual.len() != self.server.len() {
            return Err(registry_error(
                "registry must contain each approved server exactly once",
            ));
        }

        for server in &self.server {
            if server.extensions.is_empty()
                || server.markers.is_empty()
                || server.command.is_empty()
            {
                return Err(registry_error(format!(
                    "server {} is missing detection or command data",
                    server.id
                )));
            }
            for extension in &server.extensions {
                let normalized = extension.trim_start_matches('.');
                if !APPROVED_EXTENSIONS.contains(&normalized) {
                    return Err(registry_error(format!(
                        "server {} contains an unapproved extension {extension}",
                        server.id
                    )));
                }
            }
            validate_relative_executable(&server.command)?;
            semver::VersionReq::parse(&server.version_req).map_err(registry_error)?;
            validate_recipe(&server.install, &server.id)?;
        }

        let expected_runtimes: BTreeSet<_> = APPROVED_RUNTIME_IDS.into_iter().collect();
        let actual_runtimes: BTreeSet<_> = self
            .runtime
            .iter()
            .map(|runtime| runtime.id.as_str())
            .collect();
        if actual_runtimes != expected_runtimes || actual_runtimes.len() != self.runtime.len() {
            return Err(registry_error(
                "registry must contain each approved runtime exactly once",
            ));
        }
        for runtime in &self.runtime {
            validate_relative_executable(&runtime.executable)?;
            let requirement =
                semver::VersionReq::parse(&runtime.version_req).map_err(registry_error)?;
            let version = semver::Version::parse(&runtime.version).map_err(registry_error)?;
            if !requirement.matches(&version) {
                return Err(registry_error(format!(
                    "runtime {} version {} does not satisfy {}",
                    runtime.id, runtime.version, runtime.version_req
                )));
            }
            validate_archive(&runtime.archive, &runtime.id)?;
        }
        Ok(())
    }

    pub fn matching_extension(&self, extension: &str) -> impl Iterator<Item = &ServerDefinition> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        self.server.iter().filter(move |server| {
            server
                .extensions
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(&extension))
        })
    }

    pub fn server(&self, id: &str) -> Option<&ServerDefinition> {
        self.server.iter().find(|server| server.id == id)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerDefinition {
    pub id: String,
    pub display_name: String,
    pub language_id: String,
    pub version_req: String,
    pub extensions: Vec<String>,
    pub markers: Vec<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub version_args: Vec<String>,
    pub install: InstallRecipe,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstallRecipe {
    Archive {
        version: String,
        url: String,
        sha256: String,
        executable: String,
    },
    Npm {
        version: String,
        package: String,
        executable: String,
    },
    Go {
        version: String,
        module: String,
        executable: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDefinition {
    pub id: String,
    pub version: String,
    pub version_req: String,
    pub executable: String,
    pub version_args: Vec<String>,
    pub archive: ArchiveDefinition,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDefinition {
    pub url: String,
    pub sha256: String,
    pub executable: String,
}

fn validate_recipe(recipe: &InstallRecipe, server_id: &str) -> Result<(), ClspError> {
    match recipe {
        InstallRecipe::Archive {
            version,
            url,
            sha256,
            executable,
        } => {
            if version.is_empty() {
                return Err(registry_error(format!("{server_id} has no pinned version")));
            }
            validate_archive_fields(url, sha256, executable, server_id)
        }
        InstallRecipe::Npm {
            version,
            package,
            executable,
        } => {
            if version.is_empty() || package.is_empty() {
                return Err(registry_error(format!(
                    "{server_id} has an incomplete npm recipe"
                )));
            }
            validate_relative_executable(executable)
        }
        InstallRecipe::Go {
            version,
            module,
            executable,
        } => {
            if !version.starts_with('v') || !module.starts_with("golang.org/x/tools/gopls@") {
                return Err(registry_error(
                    "gopls recipe must pin its Go module version",
                ));
            }
            validate_relative_executable(executable)
        }
    }
}

fn validate_archive(archive: &ArchiveDefinition, id: &str) -> Result<(), ClspError> {
    validate_archive_fields(&archive.url, &archive.sha256, &archive.executable, id)
}

fn validate_archive_fields(
    url: &str,
    sha256: &str,
    executable: &str,
    id: &str,
) -> Result<(), ClspError> {
    if !url.starts_with("https://")
        || sha256.len() != 64
        || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(registry_error(format!(
            "{id} archive must have an HTTPS URL and SHA-256"
        )));
    }
    validate_relative_executable(executable)
}

fn validate_relative_executable(path: &str) -> Result<(), ClspError> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(registry_error("registry executable path is unsafe"));
    }
    Ok(())
}

fn registry_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(
        ErrorCode::InvalidConfig,
        format!("invalid built-in registry: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_is_the_closed_seven_server_set() {
        let registry = Registry::builtin().unwrap();
        assert_eq!(registry.server.len(), 7);
        assert_eq!(
            registry
                .server
                .iter()
                .map(|item| item.id.as_str())
                .collect::<BTreeSet<_>>(),
            APPROVED_IDS.into_iter().collect()
        );
    }

    #[test]
    fn matches_only_declared_extensions() {
        let registry = Registry::builtin().unwrap();
        assert_eq!(
            registry
                .matching_extension(".astro")
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            vec!["astro"]
        );
        assert_eq!(
            registry.matching_extension(".rs").next().unwrap().id,
            "rust"
        );
        assert!(registry.matching_extension("java").next().is_none());
    }

    #[test]
    fn astro_uses_the_locked_official_language_server() {
        let registry = Registry::builtin().unwrap();
        let astro = registry.server("astro").unwrap();
        assert_eq!(astro.command, "astro-ls.cmd");
        assert_eq!(astro.args, ["--stdio"]);
        assert_eq!(
            astro.markers,
            ["astro.config.js", "astro.config.mjs", "astro.config.ts"]
        );
        let InstallRecipe::Npm {
            version,
            package,
            executable,
        } = &astro.install
        else {
            panic!("Astro must use the managed npm closure");
        };
        assert_eq!(version, "2.16.13");
        assert_eq!(package, "@astrojs/language-server");
        assert_eq!(executable, "node_modules/.bin/astro-ls.cmd");
    }
}

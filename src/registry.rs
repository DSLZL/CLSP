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
const APPROVED_EXTENSIONS: [&str; 25] = [
    "astro", "c", "c++", "cc", "cjs", "cpp", "cts", "cxx", "go", "h", "h++", "hh", "hpp", "hxx",
    "js", "jsx", "mjs", "mts", "py", "pyi", "rs", "ts", "tsx", "yaml", "yml",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub server: Vec<ServerDefinition>,
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
            validate_program_basename(&server.command)?;
            let requirement =
                semver::VersionReq::parse(&server.version_req).map_err(registry_error)?;
            validate_recipe(&server.install, &server.id)?;
            if let InstallRecipe::Npm { version, .. } = &server.install
                && !requirement.matches(&semver::Version::parse(version).map_err(registry_error)?)
            {
                return Err(registry_error(format!(
                    "server {} version {} does not satisfy {}",
                    server.id, version, server.version_req
                )));
            }
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
    Npm {
        version: String,
        package: String,
        #[serde(default)]
        companions: Vec<String>,
    },
    Command {
        version: String,
        program: String,
        args: Vec<String>,
    },
    Manual {
        version: String,
        hint: String,
    },
}

fn validate_recipe(recipe: &InstallRecipe, server_id: &str) -> Result<(), ClspError> {
    match recipe {
        InstallRecipe::Npm {
            version,
            package,
            companions,
        } => {
            if semver::Version::parse(version).is_err() || validate_npm_package(package).is_err() {
                return Err(registry_error(format!(
                    "{server_id} has an incomplete npm recipe"
                )));
            }
            for companion in companions {
                validate_npm_spec(companion)?;
            }
            Ok(())
        }
        InstallRecipe::Command {
            version,
            program,
            args,
        } => {
            if version.is_empty() || version.len() > 128 {
                return Err(registry_error(format!("{server_id} has no recipe version")));
            }
            validate_program_basename(program)?;
            validate_args(args)
        }
        InstallRecipe::Manual { version, hint } => {
            if version.is_empty() || version.len() > 128 || hint.is_empty() || hint.len() > 1_024 {
                return Err(registry_error(format!(
                    "{server_id} has an incomplete manual recipe"
                )));
            }
            Ok(())
        }
    }
}

fn validate_npm_package(package: &str) -> Result<(), ClspError> {
    let valid = !package.is_empty()
        && package.len() <= 214
        && !package
            .chars()
            .any(|character| character.is_whitespace() || character == '\0')
        && if let Some(scoped) = package.strip_prefix('@') {
            scoped.split_once('/').is_some_and(|(scope, name)| {
                !scope.is_empty() && !name.is_empty() && !name.contains('/')
            })
        } else {
            !package.contains(['@', '/'])
        };
    if valid {
        Ok(())
    } else {
        Err(registry_error("invalid npm package name"))
    }
}

fn validate_npm_spec(spec: &str) -> Result<(), ClspError> {
    let (package, version) = spec
        .rsplit_once('@')
        .ok_or_else(|| registry_error("npm companion must pin an exact version"))?;
    validate_npm_package(package)?;
    semver::Version::parse(version).map_err(registry_error)?;
    Ok(())
}

fn validate_program_basename(program: &str) -> Result<(), ClspError> {
    let path = Path::new(program);
    if program.is_empty()
        || program.len() > 128
        || program.contains(['/', '\\', ':', '\0'])
        || path.components().count() != 1
    {
        return Err(registry_error("registry program must be a safe basename"));
    }
    Ok(())
}

fn validate_args(args: &[String]) -> Result<(), ClspError> {
    if args.len() > 16
        || args
            .iter()
            .any(|arg| arg.is_empty() || arg.len() > 512 || arg.contains('\0'))
    {
        return Err(registry_error(
            "registry command arguments are outside bounds",
        ));
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
        assert_eq!(astro.command, "astro-ls");
        assert_eq!(astro.args, ["--stdio"]);
        assert_eq!(
            astro.markers,
            ["astro.config.js", "astro.config.mjs", "astro.config.ts"]
        );
        let InstallRecipe::Npm {
            version,
            package,
            companions,
        } = &astro.install
        else {
            panic!("Astro must use the npm recipe");
        };
        assert_eq!(version, "2.16.13");
        assert_eq!(package, "@astrojs/language-server");
        assert_eq!(companions, &vec!["typescript@5.9.2".to_owned()]);
    }

    #[test]
    fn removed_archive_recipes_are_rejected() {
        assert!(
            toml::from_str::<InstallRecipe>(
                "kind = 'archive'\nversion = '1.0.0'\nurl = 'https://example.test/a.zip'\n"
            )
            .is_err()
        );
    }
}

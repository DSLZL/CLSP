use std::{collections::BTreeSet, path::Path};

use serde::{Deserialize, Serialize};

use crate::protocol::{ClspError, ErrorCode};

const BUILTIN: &str = include_str!("../registry/servers.toml");
const APPROVED_IDS: [&str; 32] = [
    "astro",
    "bash",
    "csharp",
    "clangd",
    "clojure-lsp",
    "dart",
    "deno",
    "elixir-ls",
    "eslint",
    "fsharp",
    "gleam",
    "gopls",
    "hls",
    "intelephense",
    "jdtls",
    "julials",
    "kotlin-ls",
    "lua-ls",
    "ocaml-lsp",
    "oxlint",
    "prisma",
    "pyright",
    "ruby-lsp",
    "rust",
    "sourcekit-lsp",
    "svelte",
    "terraform",
    "tinymist",
    "typescript",
    "vue",
    "yaml-ls",
    "zls",
];
const APPROVED_EXTENSIONS: [&str; 69] = [
    "astro", "bash", "c", "c++", "cc", "cjs", "clj", "cljc", "cljs", "cpp", "cs", "csx", "cts",
    "cxx", "dart", "edn", "ex", "exs", "fs", "fsi", "fsscript", "fsx", "gemspec", "gleam", "go",
    "h", "h++", "hh", "hpp", "hs", "hxx", "java", "jl", "js", "jsx", "ksh", "kt", "kts", "lhs",
    "lua", "mjs", "ml", "mli", "mts", "objc", "objcpp", "php", "prisma", "py", "pyi", "rake", "rb",
    "rs", "ru", "sh", "svelte", "swift", "tf", "tfvars", "ts", "tsx", "typ", "typc", "vue", "yaml",
    "yml", "zig", "zon", "zsh",
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
            let pinned_version = match &server.install {
                InstallRecipe::Npm { version, .. } | InstallRecipe::GithubZip { version, .. } => {
                    Some(version)
                }
                InstallRecipe::Command { .. } | InstallRecipe::Manual { .. } => None,
            };
            if let Some(version) = pinned_version
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

impl ServerDefinition {
    pub(crate) fn language_id_for_file(&self, file: &Path) -> &str {
        match (
            self.id.as_str(),
            file.extension().and_then(|extension| extension.to_str()),
        ) {
            ("typescript", Some(extension)) if extension.eq_ignore_ascii_case("tsx") => {
                "typescriptreact"
            }
            ("typescript", Some(extension)) if extension.eq_ignore_ascii_case("jsx") => {
                "javascriptreact"
            }
            ("typescript", Some(extension))
                if extension.eq_ignore_ascii_case("js")
                    || extension.eq_ignore_ascii_case("mjs")
                    || extension.eq_ignore_ascii_case("cjs") =>
            {
                "javascript"
            }
            ("sourcekit-lsp", Some(extension)) if extension.eq_ignore_ascii_case("objc") => {
                "objective-c"
            }
            ("sourcekit-lsp", Some(extension)) if extension.eq_ignore_ascii_case("objcpp") => {
                "objective-cpp"
            }
            ("terraform", Some(extension)) if extension.eq_ignore_ascii_case("tfvars") => {
                "terraform-vars"
            }
            ("tinymist", Some(extension)) if extension.eq_ignore_ascii_case("typc") => "typst-code",
            _ => &self.language_id,
        }
    }
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
    GithubZip {
        version: String,
        url: String,
        sha256: String,
        executable: String,
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
        InstallRecipe::GithubZip {
            version,
            url,
            sha256,
            executable,
        } => {
            let approved_host = match server_id {
                "clangd" | "tinymist" | "zls" => "github.com",
                "terraform" => "releases.hashicorp.com",
                _ => {
                    return Err(registry_error(format!(
                        "{server_id} has an invalid managed ZIP recipe"
                    )));
                }
            };
            if semver::Version::parse(version).is_err() {
                return Err(registry_error(format!(
                    "{server_id} has an invalid managed ZIP recipe"
                )));
            }
            let parsed = url::Url::parse(url).map_err(registry_error)?;
            if parsed.scheme() != "https"
                || parsed.host_str() != Some(approved_host)
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(registry_error(format!(
                    "{server_id} managed ZIP URL is not an approved HTTPS URL"
                )));
            }
            if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(registry_error(format!(
                    "{server_id} managed ZIP SHA-256 is invalid"
                )));
            }
            let executable_path = Path::new(executable);
            if executable.is_empty()
                || executable.len() > 512
                || executable_path.is_absolute()
                || executable_path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(registry_error(format!(
                    "{server_id} managed ZIP executable path is unsafe"
                )));
            }
            Ok(())
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
#[path = "../tests/unit/registry.rs"]
mod tests;

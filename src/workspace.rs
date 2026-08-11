use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

use crate::{
    config::DiscoveryConfig,
    protocol::{ClientKey, ClspError, ErrorCode},
    registry::{Registry, ServerDefinition},
};

const DENO_SERVER_ID: &str = "deno";
const GOPLS_SERVER_ID: &str = "gopls";
const JDTLS_SERVER_ID: &str = "jdtls";
const KOTLIN_LS_SERVER_ID: &str = "kotlin-ls";
const TYPESCRIPT_SERVER_ID: &str = "typescript";
const JDTLS_POM_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    normalized_root: String,
}

impl Workspace {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ClspError> {
        let root = fs::canonicalize(path.as_ref())
            .map_err(|error| path_error(ErrorCode::PathOutsideWorkspace, path.as_ref(), error))?;
        if !root.is_dir() {
            return Err(ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "workspace must be a directory",
            )
            .for_path(root));
        }
        Ok(Self {
            normalized_root: normalize_for_comparison(&root),
            root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn hash(&self) -> String {
        hex::encode(Sha256::digest(self.normalized_root.as_bytes()))
    }

    pub fn resolve_file(
        &self,
        input: impl AsRef<Path>,
        max_bytes: u64,
    ) -> Result<PathBuf, ClspError> {
        let input = input.as_ref();
        if input
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "parent traversal is not allowed",
            )
            .for_path(input));
        }
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root.join(input)
        };
        let resolved = fs::canonicalize(&candidate)
            .map_err(|error| path_error(ErrorCode::UnsupportedFile, &candidate, error))?;
        if !path_is_within(&self.normalized_root, &resolved) {
            return Err(ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "path resolves outside the workspace",
            )
            .for_path(input));
        }
        let metadata = fs::metadata(&resolved)
            .map_err(|error| path_error(ErrorCode::UnsupportedFile, &resolved, error))?;
        if !metadata.is_file() || metadata.len() > max_bytes {
            return Err(ClspError::new(
                ErrorCode::UnsupportedFile,
                "path is not a supported bounded regular file",
            )
            .for_path(resolved));
        }
        Ok(resolved)
    }

    pub fn resolve_candidate(&self, input: impl AsRef<Path>) -> Result<PathBuf, ClspError> {
        let input = input.as_ref();
        if input
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "parent traversal is not allowed",
            )
            .for_path(input));
        }
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root.join(input)
        };
        let mut ancestor = candidate.as_path();
        let mut suffix = Vec::new();
        while !ancestor.exists() {
            let name = ancestor.file_name().ok_or_else(|| {
                ClspError::new(
                    ErrorCode::PathOutsideWorkspace,
                    "candidate has no existing ancestor",
                )
                .for_path(&candidate)
            })?;
            suffix.push(name.to_owned());
            ancestor = ancestor.parent().ok_or_else(|| {
                ClspError::new(
                    ErrorCode::PathOutsideWorkspace,
                    "candidate has no existing ancestor",
                )
                .for_path(&candidate)
            })?;
        }
        let mut resolved = fs::canonicalize(ancestor)
            .map_err(|error| path_error(ErrorCode::UnsupportedFile, ancestor, error))?;
        if !path_is_within(&self.normalized_root, &resolved) {
            return Err(ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "candidate resolves outside the workspace",
            )
            .for_path(input));
        }
        for component in suffix.into_iter().rev() {
            resolved.push(component);
        }
        if normalize_for_comparison(&resolved) == self.normalized_root {
            return Err(ClspError::new(
                ErrorCode::UnsupportedFile,
                "workspace root is not an edit target",
            )
            .for_path(input));
        }
        Ok(resolved)
    }

    pub fn relative_candidate(&self, input: impl AsRef<Path>) -> Result<PathBuf, ClspError> {
        let resolved = self.resolve_candidate(input)?;
        resolved
            .strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                ClspError::new(
                    ErrorCode::PathOutsideWorkspace,
                    "candidate is not relative to the workspace",
                )
                .for_path(resolved)
            })
    }

    pub fn contains_existing(&self, path: impl AsRef<Path>) -> bool {
        fs::canonicalize(path)
            .ok()
            .is_some_and(|path| path_is_within(&self.normalized_root, &path))
    }

    pub fn discover(&self, registry: &Registry, config: &DiscoveryConfig) -> DiscoveryResult {
        let started = Instant::now();
        let budget = Duration::from_millis(config.max_initial_ms);
        let mut families = BTreeMap::<String, BTreeSet<PathBuf>>::new();
        let mut visited = 0usize;
        let mut complete = true;

        let deno_at_workspace_root = registry.server(DENO_SERVER_ID).is_some_and(|server| {
            server
                .markers
                .iter()
                .any(|marker| marker_exists(&self.root, marker))
        });
        for server in &registry.server {
            if !script_server_selected(server, deno_at_workspace_root) {
                continue;
            }
            for marker in &server.markers {
                if marker_exists(&self.root, marker) {
                    families
                        .entry(server.id.clone())
                        .or_default()
                        .insert(self.root.clone());
                    break;
                }
            }
        }

        let mut builder = WalkBuilder::new(&self.root);
        builder
            .hidden(false)
            .git_ignore(config.respect_gitignore)
            .git_global(config.respect_gitignore)
            .git_exclude(config.respect_gitignore)
            .max_depth(Some(config.max_depth));

        for entry in builder.build() {
            if visited >= config.max_entries || started.elapsed() >= budget {
                complete = false;
                break;
            }
            visited += 1;
            let Ok(entry) = entry else { continue };
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let Some(extension) = entry.path().extension().and_then(|value| value.to_str()) else {
                continue;
            };
            for server in self.matching_servers(entry.path(), extension, registry) {
                let root = nearest_root(entry.path(), &self.root, server);
                families.entry(server.id.clone()).or_default().insert(root);
            }
        }

        let matches = families
            .into_iter()
            .flat_map(|(server_id, roots)| {
                roots.into_iter().map(move |root| Detection {
                    server_id: server_id.clone(),
                    root,
                })
            })
            .collect();
        DiscoveryResult {
            matches,
            visited,
            complete,
        }
    }

    pub fn root_for_file(&self, file: &Path, server: &ServerDefinition) -> PathBuf {
        nearest_root(file, &self.root, server)
    }

    pub fn matching_servers<'a>(
        &self,
        file: &Path,
        extension: &str,
        registry: &'a Registry,
    ) -> Vec<&'a ServerDefinition> {
        let deno_root = registry
            .server(DENO_SERVER_ID)
            .and_then(|server| nearest_marked_root(file, &self.root, server));
        registry
            .matching_extension(extension)
            .filter(|server| {
                script_server_selected(server, deno_root.is_some())
                    && (server.id != JDTLS_SERVER_ID
                        || nearest_jdtls_root(file, &self.root).is_some())
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    pub server_id: String,
    pub root: PathBuf,
}

impl Detection {
    pub fn client_key(&self, artifact_version: &str, config_digest: &str) -> ClientKey {
        ClientKey {
            root: self.root.clone(),
            server_id: self.server_id.clone(),
            artifact_version: artifact_version.to_owned(),
            config_digest: config_digest.to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryResult {
    pub matches: Vec<Detection>,
    pub visited: usize,
    pub complete: bool,
}

fn nearest_root(file: &Path, workspace: &Path, server: &ServerDefinition) -> PathBuf {
    nearest_marked_root(file, workspace, server).unwrap_or_else(|| workspace.to_path_buf())
}

fn nearest_marked_root(
    file: &Path,
    workspace: &Path,
    server: &ServerDefinition,
) -> Option<PathBuf> {
    if server.id == JDTLS_SERVER_ID {
        return nearest_jdtls_root(file, workspace);
    }
    if server.id == KOTLIN_LS_SERVER_ID {
        return nearest_kotlin_root(file, workspace);
    }
    if server.id == GOPLS_SERVER_ID {
        let mut directory = file.parent();
        while let Some(candidate) = directory {
            if marker_exists(candidate, "go.work") {
                return Some(candidate.to_path_buf());
            }
            if is_workspace_root(candidate, workspace) {
                break;
            }
            directory = candidate.parent();
        }
    }

    let mut directory = file.parent();
    while let Some(candidate) = directory {
        if server
            .markers
            .iter()
            .any(|marker| marker_exists(candidate, marker))
        {
            return Some(candidate.to_path_buf());
        }
        if is_workspace_root(candidate, workspace) {
            break;
        }
        directory = candidate.parent();
    }
    None
}

fn nearest_jdtls_root(file: &Path, workspace: &Path) -> Option<PathBuf> {
    for markers in [
        &["settings.gradle", "settings.gradle.kts"][..],
        &["gradlew", "gradlew.bat"],
        &["build.gradle", "build.gradle.kts"],
    ] {
        if let Some(root) = nearest_root_with_markers(file, workspace, markers) {
            return Some(root);
        }
    }
    nearest_maven_root(file, workspace)
        .or_else(|| nearest_root_with_markers(file, workspace, &[".project", ".classpath"]))
}

fn nearest_kotlin_root(file: &Path, workspace: &Path) -> Option<PathBuf> {
    for markers in [
        &["settings.gradle.kts", "settings.gradle"][..],
        &["gradlew", "gradlew.bat"],
        &["build.gradle.kts", "build.gradle"],
        &["pom.xml"],
    ] {
        if let Some(root) = nearest_root_with_markers(file, workspace, markers) {
            return Some(root);
        }
    }
    None
}

fn nearest_root_with_markers(file: &Path, workspace: &Path, markers: &[&str]) -> Option<PathBuf> {
    let mut directory = file.parent();
    while let Some(candidate) = directory {
        if markers
            .iter()
            .any(|marker| marker_exists(candidate, marker))
        {
            return Some(candidate.to_path_buf());
        }
        if is_workspace_root(candidate, workspace) {
            break;
        }
        directory = candidate.parent();
    }
    None
}

fn nearest_maven_root(file: &Path, workspace: &Path) -> Option<PathBuf> {
    let mut root = nearest_root_with_markers(file, workspace, &["pom.xml"])?;
    while let Some(parent) = next_pom_ancestor(&root, workspace) {
        if !pom_declares_module(&parent.join("pom.xml"), &root) {
            break;
        }
        root = parent;
    }
    Some(root)
}

fn next_pom_ancestor(directory: &Path, workspace: &Path) -> Option<PathBuf> {
    if is_workspace_root(directory, workspace) {
        return None;
    }
    let mut ancestor = directory.parent();
    while let Some(candidate) = ancestor {
        if candidate.join("pom.xml").is_file() {
            return Some(candidate.to_path_buf());
        }
        if is_workspace_root(candidate, workspace) {
            break;
        }
        ancestor = candidate.parent();
    }
    None
}

fn pom_declares_module(pom: &Path, child: &Path) -> bool {
    let Ok(metadata) = fs::metadata(pom) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() > JDTLS_POM_LIMIT {
        return false;
    }
    let Ok(text) = fs::read_to_string(pom) else {
        return false;
    };
    let Some(parent) = pom.parent() else {
        return false;
    };
    let Ok(child) = fs::canonicalize(child) else {
        return false;
    };
    let text = strip_xml_comments(&text);
    xml_element_bodies(&text, "modules")
        .into_iter()
        .flat_map(|modules| xml_element_bodies(modules, "module"))
        .map(str::trim)
        .filter(|module| !module.is_empty())
        .any(|module| {
            fs::canonicalize(parent.join(module.replace('\\', "/")))
                .is_ok_and(|candidate| candidate == child)
        })
}

fn strip_xml_comments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        output.push_str(&rest[..start]);
        let Some(end) = rest[start + 4..].find("-->") else {
            return output;
        };
        rest = &rest[start + 4 + end + 3..];
    }
    output.push_str(rest);
    output
}

fn xml_element_bodies<'a>(text: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut bodies = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after_name = &rest[start + open.len()..];
        if !after_name
            .as_bytes()
            .first()
            .is_some_and(|byte| *byte == b'>' || byte.is_ascii_whitespace())
        {
            rest = &after_name[after_name.len().min(1)..];
            continue;
        }
        let Some(open_end) = after_name.find('>') else {
            break;
        };
        let body = &after_name[open_end + 1..];
        let Some(close_start) = body.find(&close) else {
            break;
        };
        bodies.push(&body[..close_start]);
        rest = &body[close_start + close.len()..];
    }
    bodies
}

fn script_server_selected(server: &ServerDefinition, deno_root: bool) -> bool {
    !matches!(
        (deno_root, server.id.as_str()),
        (true, TYPESCRIPT_SERVER_ID) | (false, DENO_SERVER_ID)
    )
}

fn marker_exists(directory: &Path, marker: &str) -> bool {
    let Some(extension) = marker.strip_prefix("*.") else {
        return directory.join(marker).exists();
    };
    fs::read_dir(directory).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .path()
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
    })
}

fn is_workspace_root(candidate: &Path, workspace: &Path) -> bool {
    candidate == workspace
        || fs::canonicalize(candidate).is_ok_and(|candidate| candidate == workspace)
}

fn path_is_within(normalized_root: &str, candidate: &Path) -> bool {
    let candidate = normalize_for_comparison(candidate);
    candidate == normalized_root
        || candidate
            .strip_prefix(normalized_root)
            .is_some_and(|rest| rest.starts_with('\\'))
}

fn normalize_for_comparison(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('/', "\\");
    while value.ends_with('\\') {
        value.pop();
    }
    value.to_lowercase()
}

fn path_error(code: ErrorCode, path: &Path, error: impl std::fmt::Display) -> ClspError {
    ClspError::new(code, error.to_string()).for_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_and_sibling_paths() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("inside.rs"), "fn main() {}").unwrap();
        fs::write(parent.path().join("outside.rs"), "").unwrap();
        let workspace = Workspace::open(&root).unwrap();

        assert!(workspace.resolve_file("inside.rs", 1_024).is_ok());
        assert_eq!(
            workspace
                .resolve_file("../outside.rs", 1_024)
                .unwrap_err()
                .code,
            ErrorCode::PathOutsideWorkspace
        );
        assert_eq!(
            workspace
                .resolve_file(parent.path().join("outside.rs"), 1_024)
                .unwrap_err()
                .code,
            ErrorCode::PathOutsideWorkspace
        );
    }

    #[test]
    fn resolves_missing_edit_targets_through_the_nearest_existing_ancestor() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let workspace = Workspace::open(&root).unwrap();

        let target = workspace.resolve_candidate("new/nested.rs").unwrap();
        assert_eq!(
            target,
            fs::canonicalize(&root).unwrap().join("new/nested.rs")
        );
        assert_eq!(
            workspace.relative_candidate(&target).unwrap(),
            PathBuf::from("new/nested.rs")
        );
        assert!(workspace.resolve_candidate("../outside.rs").is_err());
        assert!(
            workspace
                .resolve_candidate(parent.path().join("outside.rs"))
                .is_err()
        );
    }

    #[test]
    fn discovers_nearest_monorepo_roots() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("crate");
        fs::create_dir(&nested).unwrap();
        fs::write(
            nested.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'",
        )
        .unwrap();
        fs::write(nested.join("lib.rs"), "pub fn answer() -> u8 { 42 }").unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let result = workspace.discover(&Registry::builtin().unwrap(), &DiscoveryConfig::default());
        let rust = result
            .matches
            .iter()
            .find(|item| item.server_id == "rust")
            .unwrap();
        assert_eq!(rust.root, fs::canonicalize(nested).unwrap());
    }

    #[test]
    fn elixir_files_use_the_nearest_mix_root() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("apps/example");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("mix.exs"),
            "defmodule Example.MixProject do\nend",
        )
        .unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();

        for name in ["example.ex", "example.exs"] {
            let file = nested.join(name);
            fs::write(&file, "defmodule Example do\nend").unwrap();
            assert_eq!(
                workspace
                    .matching_servers(
                        &file,
                        file.extension().unwrap().to_str().unwrap(),
                        &registry
                    )
                    .into_iter()
                    .map(|server| server.id.as_str())
                    .collect::<Vec<_>>(),
                ["elixir-ls"]
            );
            assert_eq!(
                workspace.root_for_file(&file, registry.server("elixir-ls").unwrap()),
                nested
            );
        }
    }

    #[test]
    fn wildcard_markers_find_the_nearest_csharp_project() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("src/Demo");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("Demo.csproj"), "<Project />").unwrap();
        let source = nested.join("Program.cs");
        fs::write(&source, "class Program {}").unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let registry = Registry::builtin().unwrap();

        assert_eq!(
            workspace.root_for_file(&source, registry.server("csharp").unwrap()),
            nested
        );
        assert_eq!(
            workspace.root_for_file(
                &root.path().join("Loose.cs"),
                registry.server("csharp").unwrap()
            ),
            fs::canonicalize(root.path()).unwrap()
        );
        assert!(!marker_exists(root.path(), "global.json"));
        fs::write(root.path().join("global.json"), "{}").unwrap();
        assert!(marker_exists(root.path(), "global.json"));
    }

    #[test]
    fn fsharp_files_use_the_nearest_project_root() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("src/Demo");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("Demo.fsproj"), "<Project />").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();

        for name in ["Demo.fs", "Types.fsi", "Script.fsx", "Build.fsscript"] {
            let file = nested.join(name);
            fs::write(&file, "module Demo").unwrap();
            assert_eq!(
                workspace
                    .matching_servers(
                        &file,
                        file.extension().unwrap().to_str().unwrap(),
                        &registry
                    )
                    .into_iter()
                    .map(|server| server.id.as_str())
                    .collect::<Vec<_>>(),
                ["fsharp"]
            );
            assert_eq!(
                workspace.root_for_file(&file, registry.server("fsharp").unwrap()),
                nested
            );
        }
    }

    #[test]
    fn gleam_files_use_the_nearest_project_root_or_workspace() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("apps/example");
        let source_dir = nested.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(nested.join("gleam.toml"), "name = \"example\"").unwrap();
        let file = source_dir.join("main.gleam");
        fs::write(&file, "pub fn main() { Nil }").unwrap();
        let loose = root.path().join("loose.gleam");
        fs::write(&loose, "pub fn loose() { Nil }").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();

        assert_eq!(
            workspace
                .matching_servers(&file, "gleam", &registry)
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["gleam"]
        );
        let gleam = registry.server("gleam").unwrap();
        assert_eq!(workspace.root_for_file(&file, gleam), nested);
        assert_eq!(
            workspace.root_for_file(&loose, gleam),
            fs::canonicalize(root.path()).unwrap()
        );
    }

    #[test]
    fn gopls_prefers_go_work_then_the_nearest_module_marker_or_workspace() {
        let root = tempfile::tempdir().unwrap();
        let module = root.path().join("nested/module");
        fs::create_dir_all(&module).unwrap();
        let source = module.join("main.go");
        fs::write(&source, "package main").unwrap();
        fs::write(root.path().join("go.work"), "go 1.26\nuse ./nested/module").unwrap();
        fs::write(module.join("go.mod"), "module example.com/demo\ngo 1.26").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let gopls = registry.server(GOPLS_SERVER_ID).unwrap();
        let selected_root = || fs::canonicalize(workspace.root_for_file(&source, gopls)).unwrap();

        assert_eq!(selected_root(), fs::canonicalize(root.path()).unwrap());
        fs::remove_file(root.path().join("go.work")).unwrap();
        assert_eq!(selected_root(), fs::canonicalize(&module).unwrap());
        fs::remove_file(module.join("go.mod")).unwrap();
        fs::write(module.join("go.sum"), "").unwrap();
        assert_eq!(selected_root(), fs::canonicalize(&module).unwrap());
        fs::remove_file(module.join("go.sum")).unwrap();
        assert_eq!(selected_root(), fs::canonicalize(root.path()).unwrap());
    }

    #[test]
    fn jdtls_uses_opencode_gradle_precedence() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("app");
        let module = app.join("module");
        let source_dir = module.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(
            root.path().join("settings.gradle"),
            "rootProject.name = 'demo'",
        )
        .unwrap();
        fs::write(app.join("gradlew"), "").unwrap();
        fs::write(module.join("build.gradle"), "").unwrap();
        let source = source_dir.join("Main.java");
        fs::write(&source, "class Main {}").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let jdtls = registry.server(JDTLS_SERVER_ID).unwrap();

        assert_eq!(workspace.root_for_file(&source, jdtls), root.path());
        fs::remove_file(root.path().join("settings.gradle")).unwrap();
        assert_eq!(workspace.root_for_file(&source, jdtls), app);
        fs::remove_file(app.join("gradlew")).unwrap();
        assert_eq!(workspace.root_for_file(&source, jdtls), module);
    }

    #[test]
    fn jdtls_climbs_only_declared_maven_modules() {
        let root = tempfile::tempdir().unwrap();
        let module = root.path().join("apps/demo");
        let source_dir = module.join("src/main/java");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(module.join("pom.xml"), "<project />").unwrap();
        fs::write(
            root.path().join("pom.xml"),
            "<project><modules><!-- <module>apps/demo</module> --><module>apps/sibling</module></modules></project>",
        )
        .unwrap();
        let source = source_dir.join("Main.java");
        fs::write(&source, "class Main {}").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let jdtls = registry.server(JDTLS_SERVER_ID).unwrap();

        assert_eq!(workspace.root_for_file(&source, jdtls), module);
        fs::write(
            root.path().join("pom.xml"),
            "<project><modules><module>apps/demo</module></modules></project>",
        )
        .unwrap();
        assert_eq!(workspace.root_for_file(&source, jdtls), root.path());
    }

    #[test]
    fn jdtls_uses_eclipse_markers_but_skips_loose_java_files() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("eclipse-project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join(".classpath"), "<classpath />").unwrap();
        let source = project.join("Main.java");
        let loose = root.path().join("Loose.java");
        fs::write(&source, "class Main {}").unwrap();
        fs::write(&loose, "class Loose {}").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();

        assert_eq!(
            workspace
                .matching_servers(&source, "java", &registry)
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            [JDTLS_SERVER_ID]
        );
        assert_eq!(
            workspace.root_for_file(&source, registry.server(JDTLS_SERVER_ID).unwrap()),
            project
        );
        assert!(
            workspace
                .matching_servers(&loose, "java", &registry)
                .is_empty()
        );
    }

    #[test]
    fn jdtls_never_uses_markers_above_the_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(parent.path().join("settings.gradle"), "").unwrap();
        let source = root.join("Loose.java");
        fs::write(&source, "class Loose {}").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(&root).unwrap();

        assert!(
            workspace
                .matching_servers(&source, "java", &registry)
                .is_empty()
        );
    }

    #[test]
    fn kotlin_uses_opencode_root_precedence_and_workspace_boundary() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        let app = root.join("app");
        let module = app.join("module");
        let project = module.join("project");
        let source_dir = project.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(parent.path().join("settings.gradle"), "").unwrap();
        fs::write(root.join("settings.gradle.kts"), "").unwrap();
        fs::write(app.join("gradlew.bat"), "").unwrap();
        fs::write(module.join("build.gradle.kts"), "").unwrap();
        fs::write(project.join("pom.xml"), "<project />").unwrap();
        let source = source_dir.join("Main.kt");
        fs::write(&source, "fun main() = Unit").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(&root).unwrap();
        let kotlin = registry.server(KOTLIN_LS_SERVER_ID).unwrap();

        assert_eq!(workspace.root_for_file(&source, kotlin), root);
        fs::remove_file(root.join("settings.gradle.kts")).unwrap();
        assert_eq!(workspace.root_for_file(&source, kotlin), app);
        fs::remove_file(app.join("gradlew.bat")).unwrap();
        assert_eq!(workspace.root_for_file(&source, kotlin), module);
        fs::remove_file(module.join("build.gradle.kts")).unwrap();
        assert_eq!(workspace.root_for_file(&source, kotlin), project);
        fs::remove_file(project.join("pom.xml")).unwrap();
        assert_eq!(
            workspace.root_for_file(&source, kotlin),
            fs::canonicalize(&root).unwrap()
        );
    }

    #[test]
    fn haskell_files_use_the_nearest_project_root_or_workspace() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("apps/demo");
        let source_dir = project.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(project.join("demo.cabal"), "cabal-version: 3.0").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let hls = registry.server("hls").unwrap();

        for name in ["Main.hs", "Notes.lhs"] {
            let file = source_dir.join(name);
            fs::write(&file, "module Demo where").unwrap();
            assert_eq!(
                workspace
                    .matching_servers(
                        &file,
                        file.extension().unwrap().to_str().unwrap(),
                        &registry
                    )
                    .into_iter()
                    .map(|server| server.id.as_str())
                    .collect::<Vec<_>>(),
                ["hls"]
            );
            assert_eq!(workspace.root_for_file(&file, hls), project);
        }

        fs::remove_file(project.join("demo.cabal")).unwrap();
        assert_eq!(
            workspace.root_for_file(&source_dir.join("Main.hs"), hls),
            fs::canonicalize(root.path()).unwrap()
        );
    }

    #[test]
    fn julia_files_use_the_nearest_opencode_marker_or_workspace() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("packages/demo");
        let source_dir = project.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(root.path().join("Project.toml"), "[deps]").unwrap();
        fs::write(project.join("Manifest.toml"), "manifest_format = \"2.0\"").unwrap();
        let file = source_dir.join("Demo.jl");
        fs::write(&file, "module Demo\nend").unwrap();
        let pending = project.join("pending/Demo.jl");
        fs::create_dir_all(pending.parent().unwrap()).unwrap();
        let loose = root.path().join("loose/Loose.jl");
        fs::create_dir_all(loose.parent().unwrap()).unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let julials = registry.server("julials").unwrap();

        assert_eq!(
            workspace
                .matching_servers(&file, "jl", &registry)
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["julials"]
        );
        assert_eq!(workspace.root_for_file(&file, julials), source_dir);
        assert_eq!(workspace.root_for_file(&pending, julials), project);
        fs::remove_file(project.join("Manifest.toml")).unwrap();
        assert_eq!(workspace.root_for_file(&pending, julials), root.path());
        fs::remove_file(root.path().join("Project.toml")).unwrap();
        assert_eq!(
            workspace.root_for_file(&loose, julials),
            fs::canonicalize(root.path()).unwrap()
        );
    }

    #[test]
    fn lua_files_use_the_nearest_opencode_marker_within_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace_root = root.path().join("workspace");
        let project = workspace_root.join("packages/demo");
        let source_dir = project.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(root.path().join(".luarc.json"), "{}").unwrap();
        let file = source_dir.join("main.lua");
        fs::write(&file, "local value = 1").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(&workspace_root).unwrap();
        let lua = registry.server("lua-ls").unwrap();

        assert_eq!(
            workspace
                .matching_servers(&file, "lua", &registry)
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["lua-ls"]
        );
        for marker in &lua.markers {
            let marker = project.join(marker);
            fs::write(&marker, "{}").unwrap();
            assert_eq!(workspace.root_for_file(&file, lua), project);
            fs::remove_file(marker).unwrap();
        }
        assert_eq!(
            workspace.root_for_file(&file, lua),
            fs::canonicalize(&workspace_root).unwrap()
        );
    }

    #[test]
    fn ocaml_files_use_the_nearest_opencode_marker_within_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace_root = root.path().join("workspace");
        let project = workspace_root.join("packages/demo");
        let source_dir = project.join("lib");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(root.path().join("opam"), "").unwrap();
        fs::write(workspace_root.join("dune-project"), "(lang dune 3.0)").unwrap();
        fs::write(project.join(".merlin"), "B _build/default").unwrap();
        let registry = Registry::builtin().unwrap();
        let workspace = Workspace::open(&workspace_root).unwrap();
        let ocaml = registry.server("ocaml-lsp").unwrap();

        for name in ["demo.ml", "demo.mli"] {
            let file = source_dir.join(name);
            fs::write(&file, "let value = 1").unwrap();
            assert_eq!(
                workspace
                    .matching_servers(
                        &file,
                        file.extension().unwrap().to_str().unwrap(),
                        &registry
                    )
                    .into_iter()
                    .map(|server| server.id.as_str())
                    .collect::<Vec<_>>(),
                ["ocaml-lsp"]
            );
            assert_eq!(workspace.root_for_file(&file, ocaml), project);
        }

        fs::remove_file(project.join(".merlin")).unwrap();
        assert_eq!(
            workspace.root_for_file(&source_dir.join("demo.ml"), ocaml),
            workspace_root
        );
        fs::remove_file(workspace_root.join("dune-project")).unwrap();
        assert_eq!(
            workspace.root_for_file(&source_dir.join("demo.ml"), ocaml),
            fs::canonicalize(&workspace_root).unwrap()
        );
    }

    #[test]
    fn deno_replaces_typescript_while_eslint_coexists_at_the_nearest_lock_root() {
        let root = tempfile::tempdir().unwrap();
        let deno_root = root.path().join("deno-app");
        let node_root = root.path().join("node-app");
        fs::create_dir_all(&deno_root).unwrap();
        fs::create_dir_all(&node_root).unwrap();
        fs::write(deno_root.join("deno.json"), "{}").unwrap();
        fs::write(deno_root.join("package.json"), "{}").unwrap();
        fs::write(deno_root.join("bun.lock"), "").unwrap();
        fs::write(node_root.join("package.json"), "{}").unwrap();
        fs::write(node_root.join("bun.lock"), "").unwrap();
        let deno_file = deno_root.join("main.ts");
        let deno_js_file = deno_root.join("main.js");
        let node_file = node_root.join("main.ts");
        fs::write(&deno_file, "export const deno = true;").unwrap();
        fs::write(&deno_js_file, "export const deno = true;").unwrap();
        fs::write(&node_file, "export const node = true;").unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let registry = Registry::builtin().unwrap();

        assert_eq!(
            workspace
                .matching_servers(&deno_file, "ts", &registry)
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            vec!["deno", "eslint"]
        );
        assert_eq!(
            workspace
                .matching_servers(&deno_js_file, "js", &registry)
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            vec!["deno", "eslint"]
        );
        assert_eq!(
            workspace.root_for_file(&deno_file, registry.server("deno").unwrap()),
            deno_root
        );
        assert_eq!(
            workspace.root_for_file(&deno_file, registry.server("eslint").unwrap()),
            deno_root
        );
        assert_eq!(
            workspace
                .matching_servers(&node_file, "ts", &registry)
                .into_iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            vec!["eslint", "typescript"]
        );
        assert_eq!(
            workspace.root_for_file(&node_file, registry.server("eslint").unwrap()),
            node_root
        );
    }

    #[test]
    fn path_comparison_requires_a_component_boundary() {
        let root = normalize_for_comparison(Path::new("C:/work/project"));
        assert!(path_is_within(
            &root,
            Path::new("C:/work/project/src/lib.rs")
        ));
        assert!(!path_is_within(
            &root,
            Path::new("C:/work/project-other/lib.rs")
        ));
    }
}

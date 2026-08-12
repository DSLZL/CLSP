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
            for (_, detection) in self.detect_file(entry.path(), extension, registry) {
                families
                    .entry(detection.server_id)
                    .or_default()
                    .insert(detection.root);
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

    pub(crate) fn detect_file<'a>(
        &self,
        file: &Path,
        extension: &str,
        registry: &'a Registry,
    ) -> Vec<(&'a ServerDefinition, Detection)> {
        self.matching_servers(file, extension, registry)
            .into_iter()
            .map(|server| {
                (
                    server,
                    Detection {
                        server_id: server.id.clone(),
                        root: self.root_for_file(file, server),
                    },
                )
            })
            .collect()
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
#[path = "../tests/unit/workspace.rs"]
mod tests;

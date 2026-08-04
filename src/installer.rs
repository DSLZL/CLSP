use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use semver::{Version, VersionReq};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::AsyncWriteExt,
    process::Command,
    sync::{Mutex, Semaphore},
    time::timeout,
};

use crate::{
    config::{Config, RuntimePolicy},
    protocol::{ClientKey, ClspError, ErrorCode},
    registry::{ArchiveDefinition, InstallRecipe, Registry, ServerDefinition},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const NPM_PACKAGE_JSON: &[u8] = include_bytes!("../registry/npm/package.json");
const NPM_PACKAGE_LOCK: &[u8] = include_bytes!("../registry/npm/package-lock.json");
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct StatePaths {
    pub cache: PathBuf,
    pub runtimes: PathBuf,
    pub artifacts: PathBuf,
    pub downloads: PathBuf,
    pub workspace_state: PathBuf,
    pub logs: PathBuf,
}

impl StatePaths {
    pub fn for_workspace(workspace_hash: &str) -> Result<Self, ClspError> {
        let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            ClspError::new(
                ErrorCode::InvalidConfig,
                "LOCALAPPDATA is required on Windows",
            )
        })?;
        let root = PathBuf::from(local).join("clsp");
        let cache = root.join("cache");
        let workspace_state = root.join("state").join("workspaces").join(workspace_hash);
        let paths = Self {
            runtimes: cache.join("runtimes"),
            artifacts: cache.join("artifacts"),
            downloads: cache.join("downloads"),
            logs: workspace_state.join("logs"),
            workspace_state,
            cache,
        };
        for path in [
            &paths.runtimes,
            &paths.artifacts,
            &paths.downloads,
            &paths.workspace_state,
            &paths.logs,
        ] {
            std::fs::create_dir_all(path).map_err(artifact_error)?;
        }
        cleanup_stale_entries(&paths.downloads, false, Duration::from_secs(24 * 60 * 60));
        cleanup_stale_entries(&paths.artifacts, true, Duration::from_secs(24 * 60 * 60));
        cleanup_stale_entries(&paths.runtimes, true, Duration::from_secs(24 * 60 * 60));
        cleanup_stale_entries(
            &paths.workspace_state,
            false,
            Duration::from_secs(24 * 60 * 60),
        );
        Ok(paths)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableSource {
    ProjectLocal,
    Explicit,
    Path,
    Cache,
    Managed,
}

#[derive(Clone, Debug)]
pub struct ResolvedExecutable {
    pub path: PathBuf,
    pub version_output: String,
    pub source: ExecutableSource,
}

pub struct ArtifactManager {
    config: Config,
    registry: Registry,
    paths: StatePaths,
    client: reqwest::Client,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    installs: Semaphore,
    npm_installs: Semaphore,
}

impl ArtifactManager {
    pub fn new(config: Config, registry: Registry, paths: StatePaths) -> Result<Self, ClspError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(config.install.download_timeout_seconds))
            .build()
            .map_err(artifact_error)?;
        Ok(Self {
            installs: Semaphore::new(config.install.max_concurrency),
            npm_installs: Semaphore::new(1),
            config,
            registry,
            paths,
            client,
            locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    pub async fn resolve_server<F, Fut>(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        explicit: Option<&Path>,
        policy: RuntimePolicy,
        on_install: F,
    ) -> Result<ResolvedExecutable, ClspError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        if policy != RuntimePolicy::ManagedOnly {
            for (source, candidate) in local_candidates(server, workspace, explicit) {
                if let Ok(version_output) = self.probe_server(server, &candidate, workspace).await {
                    return Ok(ResolvedExecutable {
                        path: candidate,
                        version_output,
                        source,
                    });
                }
            }
        }

        let managed_root = self.managed_server_root(server);
        let managed_path = self.managed_server_path(server);
        if artifact_ready(&managed_root, &managed_path)
            && let Ok(version_output) = self.probe_server(server, &managed_path, workspace).await
        {
            return Ok(ResolvedExecutable {
                path: managed_path,
                version_output,
                source: ExecutableSource::Cache,
            });
        }
        if policy == RuntimePolicy::LocalOnly {
            return Err(ClspError::new(
                ErrorCode::RuntimeUnavailable,
                format!(
                    "{} is unavailable under local-only policy",
                    server.display_name
                ),
            )
            .for_server(&server.id));
        }
        if !self.config.auto_install {
            return Err(ClspError::new(
                ErrorCode::ArtifactUnavailable,
                format!(
                    "managed installation is disabled for {}",
                    server.display_name
                ),
            )
            .for_server(&server.id));
        }

        on_install().await;
        let path = self.ensure_server(server).await?;
        let version_output = self.probe_server(server, &path, workspace).await?;
        Ok(ResolvedExecutable {
            path,
            version_output,
            source: ExecutableSource::Managed,
        })
    }

    pub async fn ensure_server(&self, server: &ServerDefinition) -> Result<PathBuf, ClspError> {
        match &server.install {
            InstallRecipe::Archive {
                version,
                url,
                sha256,
                executable,
            } => {
                let final_dir = self.paths.artifacts.join(&server.id).join(version);
                self.ensure_archive(
                    &format!("server:{}:{version}", server.id),
                    &final_dir,
                    &ArchiveDefinition {
                        url: url.clone(),
                        sha256: sha256.clone(),
                        executable: executable.clone(),
                    },
                    Some((&server.version_args, server.version_req.as_str())),
                )
                .await
            }
            InstallRecipe::Npm { executable, .. } => {
                let root = self.ensure_npm_bundle().await?;
                Ok(root.join(executable))
            }
            InstallRecipe::Go {
                version,
                module,
                executable,
            } => {
                self.ensure_go(
                    &server.id,
                    version,
                    module,
                    executable,
                    &server.version_args,
                    &server.version_req,
                )
                .await
            }
        }
    }

    pub async fn write_workspace_lock<'a>(
        &self,
        resolutions: impl IntoIterator<Item = (&'a ClientKey, &'a ResolvedExecutable)>,
    ) -> Result<(), ClspError> {
        #[derive(Serialize)]
        struct Entry<'a> {
            server_id: &'a str,
            root: &'a Path,
            artifact_version: &'a str,
            executable: &'a Path,
            version: &'a str,
            source: &'a str,
        }
        let entries: Vec<_> = resolutions
            .into_iter()
            .map(|(key, resolution)| Entry {
                server_id: &key.server_id,
                root: &key.root,
                artifact_version: &key.artifact_version,
                executable: &resolution.path,
                version: &resolution.version_output,
                source: match resolution.source {
                    ExecutableSource::ProjectLocal => "project-local",
                    ExecutableSource::Explicit => "explicit",
                    ExecutableSource::Path => "path",
                    ExecutableSource::Cache => "cache",
                    ExecutableSource::Managed => "managed",
                },
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&entries).map_err(artifact_error)?;
        atomic_write(&self.paths.workspace_state.join("lsp.lock"), &bytes).await
    }

    async fn ensure_archive(
        &self,
        key: &str,
        final_dir: &Path,
        archive: &ArchiveDefinition,
        probe: Option<(&[String], &str)>,
    ) -> Result<PathBuf, ClspError> {
        let executable = final_dir.join(&archive.executable);
        if artifact_ready(final_dir, &executable) {
            return Ok(executable);
        }
        let lock = self.artifact_lock(key).await;
        let _guard = lock.lock().await;
        if artifact_ready(final_dir, &executable) {
            return Ok(executable);
        }
        reject_incomplete_artifact(final_dir)?;
        let _permit = self.installs.acquire().await.map_err(artifact_error)?;
        let temp = temporary_sibling(final_dir);
        tokio::fs::create_dir_all(&temp)
            .await
            .map_err(artifact_error)?;
        let part = temporary_download(&self.paths.downloads, key);

        let result = async {
            self.download_verified(&archive.url, &archive.sha256, &part)
                .await?;
            extract_archive(
                part.clone(),
                temp.clone(),
                archive.url.clone(),
                self.config.install.max_download_bytes,
            )
            .await?;
            let temp_executable = temp.join(&archive.executable);
            if !temp_executable.is_file() {
                return Err(ClspError::new(
                    ErrorCode::ArtifactUnavailable,
                    "archive does not contain the declared executable",
                )
                .for_path(temp_executable));
            }
            if let Some((args, requirement)) = probe {
                self.probe_compatible(&temp_executable, args, &temp, requirement)
                    .await?;
            }
            write_metadata(&temp, key, &archive.url, &archive.sha256).await?;
            publish_directory(&temp, final_dir).await
        }
        .await;

        let _ = tokio::fs::remove_file(&part).await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temp).await;
        }
        result?;
        if !artifact_ready(final_dir, &executable) {
            return Err(artifact_error("published artifact is incomplete"));
        }
        Ok(executable)
    }

    async fn ensure_runtime(&self, id: &str) -> Result<PathBuf, ClspError> {
        let runtime = self
            .registry
            .runtime
            .iter()
            .find(|runtime| runtime.id == id)
            .ok_or_else(|| {
                ClspError::new(ErrorCode::InvalidConfig, format!("unknown runtime {id}"))
            })?;
        let final_dir = self.paths.runtimes.join(&runtime.id).join(&runtime.version);
        self.ensure_archive(
            &format!("runtime:{}:{}", runtime.id, runtime.version),
            &final_dir,
            &runtime.archive,
            (runtime.id == "node").then_some((
                runtime.version_args.as_slice(),
                runtime.version_req.as_str(),
            )),
        )
        .await
    }

    async fn ensure_node(&self) -> Result<PathBuf, ClspError> {
        let runtime = self
            .registry
            .runtime
            .iter()
            .find(|runtime| runtime.id == "node")
            .ok_or_else(|| artifact_error("Node runtime is missing from the registry"))?;
        if self.config.runtime.policy != RuntimePolicy::ManagedOnly
            && let Ok(path) = which::which("node")
            && self
                .probe_compatible(
                    &path,
                    &runtime.version_args,
                    Path::new("."),
                    &runtime.version_req,
                )
                .await
                .is_ok()
        {
            return Ok(path);
        }
        if self.config.runtime.policy == RuntimePolicy::LocalOnly {
            return Err(ClspError::new(
                ErrorCode::RuntimeUnavailable,
                "a compatible Node runtime is required",
            ));
        }
        let path = self.ensure_runtime("node").await?;
        self.probe_compatible(
            &path,
            &runtime.version_args,
            Path::new("."),
            &runtime.version_req,
        )
        .await?;
        Ok(path)
    }

    async fn ensure_npm_bundle(&self) -> Result<PathBuf, ClspError> {
        let digest = hex::encode(Sha256::digest(NPM_PACKAGE_LOCK));
        let final_dir = self.paths.artifacts.join("npm-lsp").join(&digest[..16]);
        if self.npm_bundle_ready(&final_dir).await {
            return Ok(final_dir);
        }
        let node = self.ensure_node().await?;
        let npm_cli = self.ensure_runtime("npm-cli").await?;
        let npm_runtime = self
            .registry
            .runtime
            .iter()
            .find(|runtime| runtime.id == "npm-cli")
            .ok_or_else(|| artifact_error("npm CLI runtime is missing from the registry"))?;
        let mut npm_version_args = vec![npm_cli.to_string_lossy().into_owned()];
        npm_version_args.extend(npm_runtime.version_args.iter().cloned());
        self.probe_compatible(
            &node,
            &npm_version_args,
            Path::new("."),
            &npm_runtime.version_req,
        )
        .await?;
        let lock = self.artifact_lock(&format!("npm-lsp:{digest}")).await;
        let _guard = lock.lock().await;
        if self.npm_bundle_ready(&final_dir).await {
            return Ok(final_dir);
        }
        reject_incomplete_artifact(&final_dir)?;
        let _install_permit = self.installs.acquire().await.map_err(artifact_error)?;
        let _npm_permit = self.npm_installs.acquire().await.map_err(artifact_error)?;
        let temp = temporary_sibling(&final_dir);
        tokio::fs::create_dir_all(&temp)
            .await
            .map_err(artifact_error)?;

        let result = async {
            tokio::fs::write(temp.join("package.json"), NPM_PACKAGE_JSON)
                .await
                .map_err(artifact_error)?;
            tokio::fs::write(temp.join("package-lock.json"), NPM_PACKAGE_LOCK)
                .await
                .map_err(artifact_error)?;
            let user_config = temp.join("empty-user.npmrc");
            let global_config = temp.join("empty-global.npmrc");
            tokio::fs::write(&user_config, b"")
                .await
                .map_err(artifact_error)?;
            tokio::fs::write(&global_config, b"")
                .await
                .map_err(artifact_error)?;
            let cache = self.paths.cache.join("npm-cache");
            tokio::fs::create_dir_all(&cache)
                .await
                .map_err(artifact_error)?;

            let mut command = Command::new(&node);
            command
                .arg(&npm_cli)
                .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"])
                .current_dir(&temp);
            sanitize_command(&mut command);
            command
                .env("NPM_CONFIG_USERCONFIG", &user_config)
                .env("NPM_CONFIG_GLOBALCONFIG", &global_config)
                .env("NPM_CONFIG_CACHE", &cache)
                .env("NPM_CONFIG_REGISTRY", "https://registry.npmjs.org/")
                .env("NPM_CONFIG_IGNORE_SCRIPTS", "true")
                .env("NPM_CONFIG_AUDIT", "false")
                .env("NPM_CONFIG_FUND", "false");
            let output = timeout(Duration::from_secs(180), command.output())
                .await
                .map_err(|_| artifact_error("npm ci timed out"))?
                .map_err(artifact_error)?;
            if !output.status.success() {
                return Err(artifact_error(format!(
                    "npm ci failed: {}",
                    bounded_stderr(&output.stderr)
                )));
            }
            self.validate_npm_bundle(&temp).await?;
            write_metadata(&temp, "npm-lsp", "locked package-lock.json", &digest).await?;
            publish_directory(&temp, &final_dir).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temp).await;
        }
        result?;
        if !self.npm_bundle_ready(&final_dir).await {
            return Err(artifact_error("published npm bundle is incomplete"));
        }
        Ok(final_dir)
    }

    async fn ensure_go(
        &self,
        server_id: &str,
        version: &str,
        module: &str,
        executable: &str,
        version_args: &[String],
        version_req: &str,
    ) -> Result<PathBuf, ClspError> {
        let final_dir = self.paths.artifacts.join(server_id).join(version);
        let final_executable = final_dir.join(executable);
        if artifact_ready(&final_dir, &final_executable) {
            return Ok(final_executable);
        }
        let go = which::which("go").map_err(|_| {
            ClspError::new(
                ErrorCode::RuntimeUnavailable,
                "Go is required to install gopls and is not managed by CLSP",
            )
        })?;
        let lock = self.artifact_lock(&format!("go:{module}")).await;
        let _guard = lock.lock().await;
        if artifact_ready(&final_dir, &final_executable) {
            return Ok(final_executable);
        }
        reject_incomplete_artifact(&final_dir)?;
        let _permit = self.installs.acquire().await.map_err(artifact_error)?;
        let temp = temporary_sibling(&final_dir);
        tokio::fs::create_dir_all(&temp)
            .await
            .map_err(artifact_error)?;
        let mut command = Command::new(go);
        command.arg("install").arg(module).env("GOBIN", &temp);
        sanitize_command(&mut command);
        command.env("GOBIN", &temp);
        let result = async {
            let output = timeout(Duration::from_secs(180), command.output())
                .await
                .map_err(|_| artifact_error("go install timed out"))?
                .map_err(artifact_error)?;
            if !output.status.success() || !temp.join(executable).is_file() {
                return Err(artifact_error(format!(
                    "go install failed: {}",
                    bounded_stderr(&output.stderr)
                ))
                .for_server(server_id));
            }
            self.probe_compatible(&temp.join(executable), version_args, &temp, version_req)
                .await?;
            write_metadata(&temp, server_id, module, version).await?;
            publish_directory(&temp, &final_dir).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temp).await;
        }
        result?;
        if !artifact_ready(&final_dir, &final_executable) {
            return Err(artifact_error("published gopls artifact is incomplete"));
        }
        Ok(final_executable)
    }

    fn managed_server_root(&self, server: &ServerDefinition) -> PathBuf {
        match &server.install {
            InstallRecipe::Archive { version, .. } | InstallRecipe::Go { version, .. } => {
                self.paths.artifacts.join(&server.id).join(version)
            }
            InstallRecipe::Npm { .. } => {
                let digest = hex::encode(Sha256::digest(NPM_PACKAGE_LOCK));
                self.paths.artifacts.join("npm-lsp").join(&digest[..16])
            }
        }
    }

    fn managed_server_path(&self, server: &ServerDefinition) -> PathBuf {
        let executable = match &server.install {
            InstallRecipe::Archive { executable, .. }
            | InstallRecipe::Npm { executable, .. }
            | InstallRecipe::Go { executable, .. } => executable,
        };
        self.managed_server_root(server).join(executable)
    }

    async fn artifact_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn download_verified(
        &self,
        url: &str,
        expected_sha256: &str,
        destination: &Path,
    ) -> Result<(), ClspError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(artifact_error)?
            .error_for_status()
            .map_err(artifact_error)?;
        if response
            .content_length()
            .is_some_and(|length| length > self.config.install.max_download_bytes)
        {
            return Err(artifact_error("artifact exceeds configured download limit"));
        }
        let mut file = tokio::fs::File::create(destination)
            .await
            .map_err(artifact_error)?;
        let mut stream = response.bytes_stream();
        let mut digest = Sha256::new();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(artifact_error)?;
            total = total.saturating_add(chunk.len() as u64);
            if total > self.config.install.max_download_bytes {
                return Err(artifact_error("artifact exceeds configured download limit"));
            }
            digest.update(&chunk);
            file.write_all(&chunk).await.map_err(artifact_error)?;
        }
        file.flush().await.map_err(artifact_error)?;
        let actual = hex::encode(digest.finalize());
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            return Err(ClspError::new(
                ErrorCode::IntegrityFailure,
                format!("artifact SHA-256 mismatch: expected {expected_sha256}, got {actual}"),
            ));
        }
        Ok(())
    }

    async fn probe(
        &self,
        executable: &Path,
        args: &[String],
        working_dir: &Path,
    ) -> Result<String, ClspError> {
        if !executable.is_file() {
            return Err(artifact_error("executable candidate is not a file"));
        }
        let mut command = Command::new(executable);
        command.args(args).current_dir(working_dir);
        sanitize_command(&mut command);
        let output = timeout(
            Duration::from_millis(self.config.runtime.probe_timeout_ms),
            command.output(),
        )
        .await
        .map_err(|_| artifact_error("executable probe timed out"))?
        .map_err(artifact_error)?;
        if !output.status.success() {
            return Err(artifact_error(format!(
                "executable probe failed: {}",
                bounded_stderr(&output.stderr)
            )));
        }
        let text = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        Ok(String::from_utf8_lossy(text)
            .trim()
            .chars()
            .take(512)
            .collect())
    }

    async fn probe_compatible(
        &self,
        executable: &Path,
        args: &[String],
        working_dir: &Path,
        requirement: &str,
    ) -> Result<String, ClspError> {
        let output = self.probe(executable, args, working_dir).await?;
        validate_version_output(&output, requirement)?;
        Ok(output)
    }

    async fn probe_server(
        &self,
        server: &ServerDefinition,
        executable: &Path,
        working_dir: &Path,
    ) -> Result<String, ClspError> {
        match &server.install {
            InstallRecipe::Npm { package, .. } => {
                probe_npm_package(executable, package, &server.version_req).await
            }
            _ => {
                self.probe_compatible(
                    executable,
                    &server.version_args,
                    working_dir,
                    &server.version_req,
                )
                .await
            }
        }
    }

    async fn validate_npm_bundle(&self, root: &Path) -> Result<(), ClspError> {
        for server in &self.registry.server {
            if let InstallRecipe::Npm { executable, .. } = &server.install {
                self.probe_server(server, &root.join(executable), root)
                    .await?;
            }
        }
        Ok(())
    }

    async fn npm_bundle_ready(&self, root: &Path) -> bool {
        root.join("artifact.json").is_file() && self.validate_npm_bundle(root).await.is_ok()
    }
}

fn validate_version_output(output: &str, requirement: &str) -> Result<Version, ClspError> {
    let version = parse_version(output).ok_or_else(|| {
        artifact_error(format!(
            "executable version probe returned no semantic version: {output}"
        ))
    })?;
    let requirement = VersionReq::parse(requirement).map_err(artifact_error)?;
    if !requirement.matches(&version) {
        return Err(artifact_error(format!(
            "executable version {version} does not satisfy {requirement}"
        )));
    }
    Ok(version)
}

fn parse_version(output: &str) -> Option<Version> {
    output
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+'))
        })
        .filter_map(|candidate| {
            let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
            Version::parse(candidate).ok()
        })
        .next()
}

async fn probe_npm_package(
    executable: &Path,
    package: &str,
    requirement: &str,
) -> Result<String, ClspError> {
    if !executable.is_file() {
        return Err(artifact_error("executable candidate is not a file"));
    }
    for manifest in npm_package_manifest_candidates(executable, package) {
        let Ok(bytes) = tokio::fs::read(&manifest).await else {
            continue;
        };
        if bytes.len() > 1024 * 1024 {
            return Err(artifact_error("npm package manifest exceeds limit"));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(artifact_error)?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| artifact_error("npm package manifest has no version"))?;
        let parsed = validate_version_output(version, requirement)?;
        return Ok(format!("{package} {parsed}"));
    }
    Err(artifact_error(format!(
        "cannot locate package metadata for {package}"
    )))
}

fn npm_package_manifest_candidates(executable: &Path, package: &str) -> Vec<PathBuf> {
    let mut candidates = BTreeSet::new();
    for ancestor in executable.ancestors().skip(1).take(8) {
        candidates.insert(
            ancestor
                .join("node_modules")
                .join(package)
                .join("package.json"),
        );
        if ancestor
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("node_modules"))
        {
            candidates.insert(ancestor.join(package).join("package.json"));
        }
    }
    candidates.into_iter().collect()
}

fn artifact_ready(root: &Path, executable: &Path) -> bool {
    root.join("artifact.json").is_file() && executable.is_file()
}

fn reject_incomplete_artifact(root: &Path) -> Result<(), ClspError> {
    if root.exists() {
        Err(artifact_error(format!(
            "cached artifact is incomplete: {}",
            root.display()
        )))
    } else {
        Ok(())
    }
}

fn local_candidates<'a>(
    server: &'a ServerDefinition,
    workspace: &'a Path,
    explicit: Option<&'a Path>,
) -> Vec<(ExecutableSource, PathBuf)> {
    let mut candidates = Vec::new();
    for base in [
        workspace.join("node_modules").join(".bin"),
        workspace.join(".venv").join("Scripts"),
        workspace.join("bin"),
    ] {
        candidates.push((ExecutableSource::ProjectLocal, base.join(&server.command)));
    }
    if let Some(explicit) = explicit {
        candidates.push((
            ExecutableSource::Explicit,
            if explicit.is_absolute() {
                explicit.to_path_buf()
            } else {
                workspace.join(explicit)
            },
        ));
    }
    if let Ok(path) = which::which(&server.command) {
        candidates.push((ExecutableSource::Path, path));
    }
    candidates
}

pub(crate) fn resolution_fingerprint(
    server: &ServerDefinition,
    workspace: &Path,
    explicit: Option<&Path>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(server).unwrap_or_default());
    digest.update(
        std::env::var_os("PATH")
            .unwrap_or_default()
            .to_string_lossy()
            .as_bytes(),
    );
    for (_, candidate) in local_candidates(server, workspace, explicit) {
        let canonical = std::fs::canonicalize(&candidate).unwrap_or(candidate);
        digest.update(canonical.to_string_lossy().as_bytes());
        if let Ok(metadata) = std::fs::metadata(&canonical) {
            digest.update(metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified()
                && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                digest.update(duration.as_nanos().to_le_bytes());
            }
        }
    }
    hex::encode(digest.finalize())
}

pub(crate) fn sanitize_command(command: &mut Command) {
    let preserved: Vec<_> = [
        "SystemRoot",
        "WINDIR",
        "PATH",
        "PATHEXT",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
    ]
    .into_iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
    .collect();
    command.env_clear();
    command.envs(preserved);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
}

async fn extract_archive(
    archive: PathBuf,
    destination: PathBuf,
    url: String,
    max_bytes: u64,
) -> Result<(), ClspError> {
    tokio::task::spawn_blocking(move || {
        if url.ends_with(".zip") {
            extract_zip(&archive, &destination, max_bytes)
        } else if url.ends_with(".tgz") || url.ends_with(".tar.gz") {
            extract_tar_gz(&archive, &destination, max_bytes)
        } else if url.ends_with(".gz") {
            extract_single_gzip(&archive, &destination, max_bytes)
        } else {
            Err(artifact_error("unsupported archive format"))
        }
    })
    .await
    .map_err(artifact_error)?
}

fn extract_zip(archive: &Path, destination: &Path, max_bytes: u64) -> Result<(), ClspError> {
    let file = File::open(archive).map_err(artifact_error)?;
    let mut zip = zip::ZipArchive::new(file).map_err(artifact_error)?;
    let mut total = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(artifact_error)?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(artifact_error("ZIP contains an unsafe path"));
        };
        validate_archive_path(&relative)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(artifact_error("ZIP links are not allowed"));
        }
        total = total.saturating_add(entry.size());
        if total > max_bytes {
            return Err(artifact_error("extracted ZIP exceeds configured limit"));
        }
        let target = destination.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(artifact_error)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(artifact_error)?;
        }
        let mut output = File::create(target).map_err(artifact_error)?;
        std::io::copy(&mut entry, &mut output).map_err(artifact_error)?;
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, destination: &Path, max_bytes: u64) -> Result<(), ClspError> {
    let file = File::open(archive).map_err(artifact_error)?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut total = 0u64;
    for entry in archive.entries().map_err(artifact_error)? {
        let mut entry = entry.map_err(artifact_error)?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(artifact_error(
                "TAR links and special files are not allowed",
            ));
        }
        let path = entry.path().map_err(artifact_error)?.into_owned();
        validate_archive_path(&path)?;
        total = total.saturating_add(entry.size());
        if total > max_bytes {
            return Err(artifact_error("extracted TAR exceeds configured limit"));
        }
        let target = destination.join(path);
        if kind.is_dir() {
            std::fs::create_dir_all(target).map_err(artifact_error)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(artifact_error)?;
            }
            entry.unpack(target).map_err(artifact_error)?;
        }
    }
    Ok(())
}

fn extract_single_gzip(
    archive: &Path,
    destination: &Path,
    max_bytes: u64,
) -> Result<(), ClspError> {
    let file = File::open(archive).map_err(artifact_error)?;
    let mut decoder = GzDecoder::new(file).take(max_bytes.saturating_add(1));
    let target = destination.join("artifact");
    let mut output = File::create(target).map_err(artifact_error)?;
    let written = std::io::copy(&mut decoder, &mut output).map_err(artifact_error)?;
    if written > max_bytes {
        return Err(artifact_error("extracted gzip exceeds configured limit"));
    }
    Ok(())
}

fn validate_archive_path(path: &Path) -> Result<(), ClspError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(artifact_error("archive contains an unsafe path"));
    }
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_string_lossy();
        if name.contains(':') || name.ends_with([' ', '.']) {
            return Err(artifact_error(
                "archive contains a Windows alternate stream or ambiguous name",
            ));
        }
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches([' ', '.'])
            .to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|number| {
                    matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                })
        {
            return Err(artifact_error(
                "archive contains a reserved Windows device name",
            ));
        }
    }
    Ok(())
}

async fn publish_directory(temp: &Path, final_dir: &Path) -> Result<(), ClspError> {
    if final_dir.exists() {
        tokio::fs::remove_dir_all(temp)
            .await
            .map_err(artifact_error)?;
        return Ok(());
    }
    if let Some(parent) = final_dir.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(artifact_error)?;
    }
    tokio::fs::rename(temp, final_dir)
        .await
        .map_err(artifact_error)
}

async fn write_metadata(
    directory: &Path,
    id: &str,
    source: &str,
    digest: &str,
) -> Result<(), ClspError> {
    #[derive(Serialize)]
    struct Metadata<'a> {
        id: &'a str,
        source: &'a str,
        digest: &'a str,
        target: &'a str,
    }
    let bytes = serde_json::to_vec_pretty(&Metadata {
        id,
        source,
        digest,
        target: "x86_64-pc-windows-msvc",
    })
    .map_err(artifact_error)?;
    tokio::fs::write(directory.join("artifact.json"), bytes)
        .await
        .map_err(artifact_error)
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ClspError> {
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    tokio::fs::write(&temp, bytes)
        .await
        .map_err(artifact_error)?;
    crate::ipc::atomic_replace(&temp, path).map_err(artifact_error)
}

fn temporary_sibling(final_dir: &Path) -> PathBuf {
    let name = final_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    final_dir.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn temporary_download(directory: &Path, key: &str) -> PathBuf {
    directory.join(format!(
        "{}.{}-{}.part",
        safe_key(key),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn cleanup_stale_entries(root: &Path, descend_once: bool, minimum_age: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let temporary = name.ends_with(".part") || name.contains(".tmp-");
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= minimum_age);
        if temporary && old_enough {
            if metadata.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        } else if descend_once && metadata.is_dir() {
            cleanup_stale_entries(&path, false, minimum_age);
        }
    }
}

fn safe_key(key: &str) -> String {
    key.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn bounded_stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(&stderr[..stderr.len().min(4_096)]).replace(['\r', '\n'], " ")
}

fn artifact_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::ArtifactUnavailable, error.to_string()).retryable()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_archive_paths() {
        assert!(validate_archive_path(Path::new("bin/server.exe")).is_ok());
        assert!(validate_archive_path(Path::new("../server.exe")).is_err());
        assert!(validate_archive_path(Path::new("C:/server.exe")).is_err());
        assert!(validate_archive_path(Path::new("bin/server.exe:payload")).is_err());
        assert!(validate_archive_path(Path::new("bin/server.exe.")).is_err());
    }

    #[test]
    fn temporary_names_are_unique_and_adjacent() {
        let final_dir = Path::new("C:/cache/server/1.0");
        let first = temporary_sibling(final_dir);
        let second = temporary_sibling(final_dir);
        assert_ne!(first, second);
        assert_eq!(first.parent(), final_dir.parent());

        let first = temporary_download(final_dir, "server:rust");
        let second = temporary_download(final_dir, "server:rust");
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(final_dir));
    }

    #[test]
    fn stale_cleanup_only_removes_temporary_entries() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("download.1-1.part"), b"partial").unwrap();
        std::fs::write(root.path().join("artifact.json"), b"keep").unwrap();
        cleanup_stale_entries(root.path(), false, Duration::ZERO);
        assert!(!root.path().join("download.1-1.part").exists());
        assert!(root.path().join("artifact.json").exists());
    }

    #[test]
    fn npm_lock_has_exact_https_integrity_for_every_package() {
        let lock: serde_json::Value = serde_json::from_slice(NPM_PACKAGE_LOCK).unwrap();
        let package: serde_json::Value = serde_json::from_slice(NPM_PACKAGE_JSON).unwrap();
        for (package, metadata) in lock["packages"].as_object().unwrap() {
            if package.is_empty() {
                continue;
            }
            assert!(metadata["version"].is_string(), "{package}");
            assert!(
                metadata["resolved"]
                    .as_str()
                    .is_some_and(|value| value.starts_with("https://registry.npmjs.org/")),
                "{package}"
            );
            assert!(
                metadata["integrity"]
                    .as_str()
                    .is_some_and(|value| value.starts_with("sha512-")),
                "{package}"
            );
        }
        let registry = Registry::builtin().unwrap();
        for server in &registry.server {
            if let InstallRecipe::Npm {
                version,
                package: name,
                ..
            } = &server.install
            {
                assert_eq!(package["dependencies"][name], version.as_str());
                assert_eq!(lock["packages"][""]["dependencies"][name], version.as_str());
                assert!(validate_version_output(version, &server.version_req).is_ok());
            }
        }
    }

    #[test]
    fn locates_project_and_global_npm_package_manifests() {
        let project = Path::new("C:/work/node_modules/.bin/pyright-langserver.cmd");
        assert!(
            npm_package_manifest_candidates(project, "pyright")
                .contains(&PathBuf::from("C:/work/node_modules/pyright/package.json"))
        );

        let global = Path::new("C:/Users/me/AppData/Roaming/npm/pyright-langserver.cmd");
        assert!(
            npm_package_manifest_candidates(global, "pyright").contains(&PathBuf::from(
                "C:/Users/me/AppData/Roaming/npm/node_modules/pyright/package.json"
            ))
        );
    }

    #[tokio::test]
    async fn npm_server_version_comes_from_manifest_without_running_the_wrapper() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("node_modules/.bin");
        let package = root.path().join("node_modules/pyright");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        let executable = bin.join("pyright-langserver.cmd");
        std::fs::write(&executable, b"@exit /b 99").unwrap();
        std::fs::write(package.join("package.json"), br#"{"version":"1.1.405"}"#).unwrap();

        assert_eq!(
            probe_npm_package(&executable, "pyright", ">=1.1.300, <2.0.0")
                .await
                .unwrap(),
            "pyright 1.1.405"
        );
    }

    #[test]
    fn relative_explicit_executables_are_workspace_relative() {
        let registry = Registry::builtin().unwrap();
        let candidates = local_candidates(
            registry.server("rust").unwrap(),
            Path::new("C:/work"),
            Some(Path::new("tools/rust-analyzer.exe")),
        );
        assert!(candidates.contains(&(
            ExecutableSource::Explicit,
            PathBuf::from("C:/work/tools/rust-analyzer.exe")
        )));
    }

    #[test]
    fn parses_common_language_server_versions_and_enforces_ranges() {
        for (output, expected) in [
            ("v26.5.0", Version::new(26, 5, 0)),
            ("golang.org/x/tools/gopls v0.21.1", Version::new(0, 21, 1)),
            ("clangd version 18.1.8", Version::new(18, 1, 8)),
            (
                "rust-analyzer 1.88.0 (6b00bc388 2025-06-23)",
                Version::new(1, 88, 0),
            ),
        ] {
            assert_eq!(parse_version(output), Some(expected));
        }
        assert!(validate_version_output("tool v1.4.0", ">=1.0.0, <2.0.0").is_ok());
        assert!(validate_version_output("tool v2.0.0", ">=1.0.0, <2.0.0").is_err());
    }

    #[test]
    fn executable_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let registry = Registry::builtin().unwrap();
        let server = registry.server("rust").unwrap();
        let executable = bin.join(&server.command);
        std::fs::write(&executable, b"one").unwrap();
        let first = resolution_fingerprint(server, directory.path(), None);
        std::fs::write(executable, b"different-size").unwrap();
        let second = resolution_fingerprint(server, directory.path(), None);
        assert_ne!(first, second);
    }
}

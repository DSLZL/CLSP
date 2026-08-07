use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use semver::{Version, VersionReq};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::Mutex,
    time::timeout,
};

use crate::{
    config::Config,
    protocol::{ClientKey, ClspError, ErrorCode},
    registry::{InstallRecipe, ServerDefinition},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const OUTPUT_LIMIT: usize = 4_096;
const ARCHIVE_DOWNLOAD_LIMIT: u64 = 32 * 1024 * 1024;
const ARCHIVE_EXTRACT_LIMIT: u64 = 512 * 1024 * 1024;
const ARCHIVE_ENTRY_LIMIT: usize = 4_096;
const VSCODE_INSTALL_ENTRY_LIMIT: usize = 32;
const ROSLYN_LANGUAGE_SERVER_PACKAGE: &str = "roslyn-language-server";
const PRESERVED_ENV: &[&str] = &[
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATH",
    "PATHEXT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "JAVA_HOME",
    "GOBIN",
    "GOPATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "DOTNET_CLI_HOME",
    "DOTNET_ROOT",
    "ProgramFiles(x86)",
    "PNPM_HOME",
    "NPM_CONFIG_PREFIX",
    "BUN_INSTALL",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct StatePaths {
    pub workspace_state: PathBuf,
    pub logs: PathBuf,
    pub artifacts: PathBuf,
}

impl StatePaths {
    pub fn for_workspace(workspace_hash: &str) -> Result<Self, ClspError> {
        let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            ClspError::new(
                ErrorCode::InvalidConfig,
                "LOCALAPPDATA is required on Windows",
            )
        })?;
        let clsp_root = PathBuf::from(local).join("clsp");
        let workspace_state = clsp_root
            .join("state")
            .join("workspaces")
            .join(workspace_hash);
        let paths = Self {
            logs: workspace_state.join("logs"),
            workspace_state,
            artifacts: clsp_root.join("artifacts"),
        };
        for path in [&paths.workspace_state, &paths.logs, &paths.artifacts] {
            std::fs::create_dir_all(path).map_err(server_error)?;
        }
        Ok(paths)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableSource {
    ProjectLocal,
    Explicit,
    Path,
    VsCodeExtension,
    Installed,
}

#[derive(Clone, Debug)]
pub struct ResolvedExecutable {
    pub path: PathBuf,
    pub version_output: String,
    pub source: ExecutableSource,
    pub npm_modules_root: Option<PathBuf>,
}

pub struct ServerResolver {
    config: Config,
    paths: StatePaths,
    vscode_app_data: Option<PathBuf>,
    dotnet_cli_home: Option<PathBuf>,
    install_lock: Mutex<()>,
}

impl ServerResolver {
    pub fn new(config: Config, paths: StatePaths) -> Self {
        Self {
            config,
            paths,
            vscode_app_data: std::env::var_os("APPDATA").map(PathBuf::from),
            dotnet_cli_home: dotnet_cli_home(),
            install_lock: Mutex::new(()),
        }
    }

    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    pub async fn resolve_server<F, Fut>(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        explicit: Option<&Path>,
        on_install: F,
    ) -> Result<ResolvedExecutable, ClspError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        if let Some(resolution) = self.resolve_local(server, workspace, explicit).await {
            return Ok(resolution);
        }

        if matches!(server.install, InstallRecipe::Npm { .. }) {
            let manager = self
                .select_npm_manager()
                .await
                .map_err(|error| error.for_server(&server.id))?;
            if let Some(resolution) = self
                .resolve_npm_global(server, &manager, false)
                .await
                .map_err(|error| error.for_server(&server.id))?
            {
                return Ok(resolution);
            }
        }

        if let InstallRecipe::Command { program, .. } = &server.install {
            let program = self
                .require_program(program)
                .await
                .map_err(|error| error.for_server(&server.id))?;
            if let Some(resolution) = self
                .resolve_toolchain_candidate(server, workspace, &program, false)
                .await
                .map_err(|error| error.for_server(&server.id))?
            {
                return Ok(resolution);
            }
        }

        if let InstallRecipe::GithubZip {
            version,
            executable,
            ..
        } = &server.install
            && let Some(resolution) = self
                .resolve_github_zip_existing(server, workspace, version, executable)
                .await
        {
            return Ok(resolution);
        }

        if let InstallRecipe::Manual { hint, .. } = &server.install {
            return Err(runtime_error(hint).for_server(&server.id));
        }

        if !self.config.auto_install {
            return Err(runtime_error(format!(
                "{} is unavailable and auto_install is false",
                server.display_name
            ))
            .for_server(&server.id));
        }

        let _guard = self.install_lock.lock().await;
        if let Some(resolution) = self.resolve_local(server, workspace, explicit).await {
            return Ok(resolution);
        }
        if let InstallRecipe::GithubZip {
            version,
            executable,
            ..
        } = &server.install
            && let Some(resolution) = self
                .resolve_github_zip_existing(server, workspace, version, executable)
                .await
        {
            return Ok(resolution);
        }

        match &server.install {
            InstallRecipe::Manual { hint, .. } => Err(runtime_error(hint).for_server(&server.id)),
            InstallRecipe::Npm {
                version,
                package,
                companions,
            } => {
                let manager = self
                    .select_npm_manager()
                    .await
                    .map_err(|error| error.for_server(&server.id))?;
                if let Some(resolution) = self
                    .resolve_npm_global(server, &manager, false)
                    .await
                    .map_err(|error| error.for_server(&server.id))?
                {
                    return Ok(resolution);
                }
                on_install().await;
                self.install_npm(server, &manager, package, version, companions)
                    .await
                    .map_err(|error| error.for_server(&server.id))
            }
            InstallRecipe::Command { program, args, .. } => {
                let program = self
                    .require_program(program)
                    .await
                    .map_err(|error| error.for_server(&server.id))?;
                if let Some(resolution) = self
                    .resolve_toolchain_candidate(server, workspace, &program, false)
                    .await
                    .map_err(|error| error.for_server(&server.id))?
                {
                    return Ok(resolution);
                }
                on_install().await;
                self.install_command(server, workspace, &program, args)
                    .await
                    .map_err(|error| error.for_server(&server.id))
            }
            InstallRecipe::GithubZip {
                version,
                url,
                sha256,
                executable,
            } => {
                on_install().await;
                self.install_github_zip(server, workspace, version, url, sha256, executable)
                    .await
                    .map_err(|error| error.for_server(&server.id))
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
                    ExecutableSource::VsCodeExtension => "vscode-extension",
                    ExecutableSource::Installed => "installed",
                },
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&entries).map_err(server_error)?;
        atomic_write(&self.paths.workspace_state.join("lsp.lock"), &bytes).await
    }

    async fn resolve_local(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        explicit: Option<&Path>,
    ) -> Option<ResolvedExecutable> {
        for (source, candidate) in local_candidates(server, workspace, explicit) {
            if let Ok(probe) = self.probe_server(server, &candidate, workspace).await {
                return Some(ResolvedExecutable {
                    path: candidate,
                    version_output: probe.version_output,
                    source,
                    npm_modules_root: probe.npm_modules_root,
                });
            }
        }
        None
    }

    async fn resolve_github_zip_existing(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        version: &str,
        executable: &str,
    ) -> Option<ResolvedExecutable> {
        if server.id == "clangd"
            && let Some(app_data) = &self.vscode_app_data
        {
            for candidate in vscode_clangd_candidates_from(app_data) {
                if let Some(resolution) = self
                    .resolve_candidate(
                        server,
                        workspace,
                        candidate,
                        ExecutableSource::VsCodeExtension,
                    )
                    .await
                {
                    return Some(resolution);
                }
            }
        }

        self.resolve_candidate(
            server,
            workspace,
            github_zip_candidate(&self.paths.artifacts, &server.id, version, executable),
            ExecutableSource::Installed,
        )
        .await
    }

    async fn resolve_candidate(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        candidate: PathBuf,
        source: ExecutableSource,
    ) -> Option<ResolvedExecutable> {
        let probe = self
            .probe_server(server, &candidate, workspace)
            .await
            .ok()?;
        Some(ResolvedExecutable {
            path: candidate,
            version_output: probe.version_output,
            source,
            npm_modules_root: probe.npm_modules_root,
        })
    }

    async fn install_github_zip(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        version: &str,
        url: &str,
        sha256: &str,
        executable: &str,
    ) -> Result<ResolvedExecutable, ClspError> {
        if !cfg!(all(windows, target_arch = "x86_64")) {
            return Err(runtime_error(
                "CLSP clangd self-install currently supports Windows x86-64 only; install clangd locally or set lsp.clangd.executable",
            ));
        }

        let server_root = self.paths.artifacts.join(&server.id);
        let install_root = server_root.join(version);
        if install_root.exists() {
            tokio::fs::remove_dir_all(&install_root)
                .await
                .map_err(server_error)?;
        }
        tokio::fs::create_dir_all(&server_root)
            .await
            .map_err(server_error)?;

        let suffix = format!(
            "{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let archive_path = server_root.join(format!(".{version}-{suffix}.zip.part"));
        let extraction_root = server_root.join(format!(".{version}-{suffix}.tmp"));
        let curl = system_curl()?;
        let args = vec![
            "--fail".to_owned(),
            "--location".to_owned(),
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--proto".to_owned(),
            "=https".to_owned(),
            "--proto-redir".to_owned(),
            "=https".to_owned(),
            "--max-filesize".to_owned(),
            ARCHIVE_DOWNLOAD_LIMIT.to_string(),
            "--output".to_owned(),
            archive_path.to_string_lossy().into_owned(),
            url.to_owned(),
        ];

        let install_result = async {
            run_checked(
                &curl,
                &args,
                &server_root,
                Duration::from_secs(self.config.install.command_timeout_seconds),
                &format!("{} archive download", server.display_name),
            )
            .await?;
            let archive_size = tokio::fs::metadata(&archive_path)
                .await
                .map_err(server_error)?
                .len();
            if archive_size == 0 || archive_size > ARCHIVE_DOWNLOAD_LIMIT {
                return Err(server_error(format!(
                    "{} archive is outside the {} byte limit",
                    server.display_name, ARCHIVE_DOWNLOAD_LIMIT
                )));
            }
            verify_file_sha256(&archive_path, sha256).await?;

            let archive = archive_path.clone();
            let destination = extraction_root.clone();
            tokio::task::spawn_blocking(move || {
                extract_zip(&archive, &destination, ARCHIVE_EXTRACT_LIMIT)
            })
            .await
            .map_err(server_error)??;
            if !extraction_root.join(executable).is_file() {
                return Err(server_error(format!(
                    "{} archive does not contain {executable}",
                    server.display_name
                )));
            }
            tokio::fs::rename(&extraction_root, &install_root)
                .await
                .map_err(server_error)
        }
        .await;

        let _ = tokio::fs::remove_file(&archive_path).await;
        if let Err(error) = install_result {
            let _ = tokio::fs::remove_dir_all(&extraction_root).await;
            return Err(error);
        }

        let candidate =
            github_zip_candidate(&self.paths.artifacts, &server.id, version, executable);
        let Some(resolution) = self
            .resolve_candidate(server, workspace, candidate, ExecutableSource::Installed)
            .await
        else {
            let _ = tokio::fs::remove_dir_all(&install_root).await;
            return Err(server_error(format!(
                "{} archive installed but the executable failed its version probe",
                server.display_name
            )));
        };
        Ok(resolution)
    }

    async fn select_npm_manager(&self) -> Result<NpmManagerSelection, ClspError> {
        let candidates = NpmManager::ALL.into_iter().filter_map(|manager| {
            which::which(manager.program())
                .ok()
                .map(|executable| (manager, executable))
        });
        self.select_npm_manager_from(candidates).await
    }

    async fn select_npm_manager_from(
        &self,
        candidates: impl IntoIterator<Item = (NpmManager, PathBuf)>,
    ) -> Result<NpmManagerSelection, ClspError> {
        for (manager, executable) in candidates {
            let Ok(output) = run_command(
                &executable,
                &["--version".to_owned()],
                &self.paths.workspace_state,
                Duration::from_millis(self.config.runtime.probe_timeout_ms),
            )
            .await
            else {
                continue;
            };
            if output.status.success() {
                return Ok(NpmManagerSelection {
                    manager,
                    executable,
                });
            }
        }
        Err(runtime_error(
            "npm language servers require a working local package manager; checked bun, pnpm, then npm",
        ))
    }

    async fn resolve_npm_global(
        &self,
        server: &ServerDefinition,
        manager: &NpmManagerSelection,
        installed: bool,
    ) -> Result<Option<ResolvedExecutable>, ClspError> {
        let roots = self.npm_roots(manager, installed).await?;
        if !roots.bin.is_dir() || !roots.modules.is_dir() {
            return Ok(None);
        }
        self.resolve_npm_in_roots(server, &roots, installed).await
    }

    async fn resolve_npm_in_roots(
        &self,
        server: &ServerDefinition,
        roots: &NpmRoots,
        installed: bool,
    ) -> Result<Option<ResolvedExecutable>, ClspError> {
        let Some(candidate) = executable_candidates_in(&roots.bin, &server.command)
            .into_iter()
            .find(|candidate| candidate.is_file())
        else {
            return Ok(None);
        };
        let InstallRecipe::Npm { package, .. } = &server.install else {
            return Ok(None);
        };
        let Ok(version_output) =
            probe_npm_manifest_in_root(&roots.modules, package, &server.version_req).await
        else {
            return Ok(None);
        };
        Ok(Some(ResolvedExecutable {
            path: candidate,
            version_output,
            source: if installed {
                ExecutableSource::Installed
            } else {
                ExecutableSource::Path
            },
            npm_modules_root: Some(roots.modules.clone()),
        }))
    }

    async fn install_npm(
        &self,
        server: &ServerDefinition,
        manager: &NpmManagerSelection,
        package: &str,
        version: &str,
        companions: &[String],
    ) -> Result<ResolvedExecutable, ClspError> {
        let args = npm_install_args(manager.manager, package, version, companions);
        run_checked(
            &manager.executable,
            &args,
            &self.paths.workspace_state,
            Duration::from_secs(self.config.install.command_timeout_seconds),
            &format!("{} global install", manager.manager.program()),
        )
        .await?;

        let roots = self.npm_roots(manager, true).await?;
        verify_exact_npm_manifest(&roots.modules, package, version).await?;
        for companion in companions {
            let (name, version) = companion
                .rsplit_once('@')
                .ok_or_else(|| server_error("invalid pinned npm companion"))?;
            verify_exact_npm_manifest(&roots.modules, name, version).await?;
        }
        self.resolve_npm_in_roots(server, &roots, true)
            .await?
            .ok_or_else(|| {
                server_error(format!(
                    "{} completed but {} was not found in {}",
                    manager.manager.program(),
                    server.command,
                    roots.bin.display()
                ))
            })
    }

    async fn npm_roots(
        &self,
        manager: &NpmManagerSelection,
        require_existing: bool,
    ) -> Result<NpmRoots, ClspError> {
        let duration = Duration::from_millis(self.config.runtime.probe_timeout_ms);
        let label = format!("{} root query", manager.manager.program());
        let bin_output = run_checked(
            &manager.executable,
            &manager.manager.bin_args(),
            &self.paths.workspace_state,
            duration,
            &label,
        )
        .await?;
        let modules_output = run_checked(
            &manager.executable,
            &manager.manager.modules_args(),
            &self.paths.workspace_state,
            duration,
            &label,
        )
        .await?;
        let mut bin = absolute_output_path(&bin_output.stdout, "package-manager bin root")?;
        let modules = match manager.manager {
            NpmManager::Bun => bun_modules_root(&modules_output.stdout)?,
            NpmManager::Pnpm | NpmManager::Npm => {
                absolute_output_path(&modules_output.stdout, "package-manager modules root")?
            }
        };
        if manager.manager == NpmManager::Npm && !cfg!(windows) {
            bin.push("bin");
        }
        if require_existing && (!bin.is_dir() || !modules.is_dir()) {
            return Err(server_error(format!(
                "{} reported missing global roots: bin={}, modules={}",
                manager.manager.program(),
                bin.display(),
                modules.display()
            )));
        }
        Ok(NpmRoots { bin, modules })
    }

    async fn require_program(&self, program: &str) -> Result<PathBuf, ClspError> {
        let executable = which::which(program).map_err(|_| {
            runtime_error(format!(
                "{program} is required locally; CLSP does not install prerequisite toolchains"
            ))
        })?;
        let version_args = if program == "go" {
            vec!["version".to_owned()]
        } else {
            vec!["--version".to_owned()]
        };
        let output = run_command(
            &executable,
            &version_args,
            &self.paths.workspace_state,
            Duration::from_millis(self.config.runtime.probe_timeout_ms),
        )
        .await
        .map_err(|error| {
            runtime_error(format!("{program} version probe failed: {}", error.message))
        })?;
        if !output.status.success() {
            return Err(runtime_error(format!(
                "{program} version probe failed: {}",
                command_output_detail(&output)
            )));
        }
        Ok(executable)
    }

    async fn resolve_toolchain_candidate(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        program: &Path,
        installed: bool,
    ) -> Result<Option<ResolvedExecutable>, ClspError> {
        let candidate = match server.id.as_str() {
            "csharp" => self.dotnet_tool_candidate(server, program).await?,
            "gopls" => self.gopls_candidate(program).await?,
            "rust" => self.rustup_candidate(program, workspace, installed).await?,
            _ => None,
        };
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let probe = match self.probe_server(server, &candidate, workspace).await {
            Ok(probe) => probe,
            Err(error) if installed => return Err(error),
            Err(_) => return Ok(None),
        };
        Ok(Some(ResolvedExecutable {
            path: candidate,
            version_output: probe.version_output,
            source: if installed {
                ExecutableSource::Installed
            } else {
                ExecutableSource::Path
            },
            npm_modules_root: None,
        }))
    }

    async fn install_command(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
        program: &Path,
        args: &[String],
    ) -> Result<ResolvedExecutable, ClspError> {
        let dotnet_version = match (&*server.id, &server.install) {
            ("csharp", InstallRecipe::Command { version, .. }) => Some(version.as_str()),
            _ => None,
        };
        let command_args = if dotnet_version.is_some() {
            let installed = self
                .dotnet_global_tool_version(program, ROSLYN_LANGUAGE_SERVER_PACKAGE)
                .await?
                .is_some();
            dotnet_tool_command_args(args, installed)?
        } else {
            args.to_vec()
        };
        let cwd = if server.id == "rust" {
            workspace
        } else {
            &self.paths.workspace_state
        };
        run_checked(
            program,
            &command_args,
            cwd,
            Duration::from_secs(self.config.install.command_timeout_seconds),
            &format!("{} install", server.display_name),
        )
        .await?;

        if let Some(expected) = dotnet_version {
            let actual = self
                .dotnet_global_tool_version(program, ROSLYN_LANGUAGE_SERVER_PACKAGE)
                .await?;
            if actual.as_deref() != Some(expected) {
                return Err(server_error(format!(
                    "{} installed dotnet tool version {}, expected {expected}",
                    server.display_name,
                    actual.as_deref().unwrap_or("missing")
                )));
            }
            return self
                .resolve_toolchain_candidate(server, workspace, program, true)
                .await?
                .ok_or_else(|| {
                    server_error(format!(
                        "{} install command succeeded but no compatible executable was found",
                        server.display_name
                    ))
                });
        }

        if let Some(mut resolution) = self.resolve_local(server, workspace, None).await {
            resolution.source = ExecutableSource::Installed;
            return Ok(resolution);
        }
        self.resolve_toolchain_candidate(server, workspace, program, true)
            .await?
            .ok_or_else(|| {
                server_error(format!(
                    "{} install command succeeded but no compatible executable was found",
                    server.display_name
                ))
            })
    }

    async fn gopls_candidate(&self, go: &Path) -> Result<Option<PathBuf>, ClspError> {
        let output = run_checked(
            go,
            &["env".to_owned(), "GOBIN".to_owned(), "GOPATH".to_owned()],
            &self.paths.workspace_state,
            Duration::from_millis(self.config.runtime.probe_timeout_ms),
            "go env GOBIN GOPATH",
        )
        .await?;
        let Some(bin) = go_bin_from_env_output(&output.stdout)? else {
            return Ok(None);
        };
        Ok(executable_candidates_in(&bin, "gopls")
            .into_iter()
            .find(|path| path.is_file()))
    }

    async fn dotnet_tool_candidate(
        &self,
        server: &ServerDefinition,
        dotnet: &Path,
    ) -> Result<Option<PathBuf>, ClspError> {
        let InstallRecipe::Command { version, .. } = &server.install else {
            return Ok(None);
        };
        if self
            .dotnet_global_tool_version(dotnet, ROSLYN_LANGUAGE_SERVER_PACKAGE)
            .await?
            .as_deref()
            != Some(version)
        {
            return Ok(None);
        }
        Ok(self.dotnet_cli_home.as_deref().and_then(|home| {
            dotnet_tool_candidates(home, &server.command)
                .into_iter()
                .find(|candidate| candidate.is_file())
        }))
    }

    async fn dotnet_global_tool_version(
        &self,
        dotnet: &Path,
        package: &str,
    ) -> Result<Option<String>, ClspError> {
        let output = run_command(
            dotnet,
            &[
                "tool".to_owned(),
                "list".to_owned(),
                "--global".to_owned(),
                package.to_owned(),
            ],
            &self.paths.workspace_state,
            Duration::from_millis(self.config.runtime.probe_timeout_ms),
        )
        .await?;
        let version = parse_dotnet_tool_version(&output.stdout, package)?;
        if output.status.success()
            || (output.status.code() == Some(1)
                && version.is_none()
                && output.stderr.iter().all(u8::is_ascii_whitespace))
        {
            return Ok(version);
        }
        Err(server_error(format!(
            "dotnet tool list exited with {}; {}",
            output.status,
            command_output_detail(&output)
        )))
    }

    async fn rustup_candidate(
        &self,
        rustup: &Path,
        workspace: &Path,
        required: bool,
    ) -> Result<Option<PathBuf>, ClspError> {
        let output = run_command(
            rustup,
            &["which".to_owned(), "rust-analyzer".to_owned()],
            workspace,
            Duration::from_millis(self.config.runtime.probe_timeout_ms),
        )
        .await?;
        if !output.status.success() {
            return if required {
                Err(server_error(format!(
                    "rustup which rust-analyzer failed: {}",
                    command_output_detail(&output)
                )))
            } else {
                Ok(None)
            };
        }
        let path = absolute_output_path(&output.stdout, "rustup component path")?;
        if !path.is_file() {
            return if required {
                Err(server_error(format!(
                    "rustup reported a missing rust-analyzer: {}",
                    path.display()
                )))
            } else {
                Ok(None)
            };
        }
        Ok(Some(path))
    }

    async fn probe_server(
        &self,
        server: &ServerDefinition,
        executable: &Path,
        working_dir: &Path,
    ) -> Result<ServerProbe, ClspError> {
        match &server.install {
            InstallRecipe::Npm { package, .. } => {
                let probe = probe_npm_package(executable, package, &server.version_req).await?;
                Ok(ServerProbe {
                    version_output: probe.version_output,
                    npm_modules_root: Some(probe.modules_root),
                })
            }
            InstallRecipe::Command { .. }
            | InstallRecipe::GithubZip { .. }
            | InstallRecipe::Manual { .. } => {
                let version_output = self
                    .probe_compatible(
                        executable,
                        &server.version_args,
                        working_dir,
                        &server.version_req,
                    )
                    .await?;
                Ok(ServerProbe {
                    version_output,
                    npm_modules_root: None,
                })
            }
        }
    }

    async fn probe_compatible(
        &self,
        executable: &Path,
        args: &[String],
        working_dir: &Path,
        requirement: &str,
    ) -> Result<String, ClspError> {
        if !executable.is_file() {
            return Err(server_error("executable candidate is not a file"));
        }
        let output = run_checked(
            executable,
            args,
            working_dir,
            Duration::from_millis(self.config.runtime.probe_timeout_ms),
            "executable probe",
        )
        .await?;
        let text = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        let text: String = String::from_utf8_lossy(text)
            .trim()
            .chars()
            .take(512)
            .collect();
        validate_version_output(&text, requirement)?;
        Ok(text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NpmManager {
    Bun,
    Pnpm,
    Npm,
}

impl NpmManager {
    const ALL: [Self; 3] = [Self::Bun, Self::Pnpm, Self::Npm];

    fn program(self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Pnpm => "pnpm",
            Self::Npm => "npm",
        }
    }

    fn install_args(self) -> Vec<String> {
        let args: &[&str] = match self {
            Self::Bun => &["install", "--global", "--ignore-scripts"],
            Self::Pnpm => &["add", "--global", "--ignore-scripts"],
            Self::Npm => &[
                "install",
                "--global",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
            ],
        };
        args.iter().map(|value| (*value).to_owned()).collect()
    }

    fn bin_args(self) -> Vec<String> {
        match self {
            Self::Bun => ["pm", "bin", "--global"].as_slice(),
            Self::Pnpm => ["bin", "--global"].as_slice(),
            Self::Npm => ["prefix", "--global"].as_slice(),
        }
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
    }

    fn modules_args(self) -> Vec<String> {
        match self {
            Self::Bun => ["pm", "ls", "--global"].as_slice(),
            Self::Pnpm | Self::Npm => ["root", "--global"].as_slice(),
        }
        .iter()
        .map(|value| (*value).to_owned())
        .collect()
    }
}

#[derive(Debug)]
struct NpmManagerSelection {
    manager: NpmManager,
    executable: PathBuf,
}

struct NpmRoots {
    bin: PathBuf,
    modules: PathBuf,
}

struct ServerProbe {
    version_output: String,
    npm_modules_root: Option<PathBuf>,
}

struct NpmProbe {
    version_output: String,
    modules_root: PathBuf,
}

#[derive(Debug)]
struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn validate_version_output(output: &str, requirement: &str) -> Result<Version, ClspError> {
    let version = parse_version(output).ok_or_else(|| {
        server_error(format!(
            "executable version probe returned no semantic version: {output}"
        ))
    })?;
    let requirement = VersionReq::parse(requirement).map_err(server_error)?;
    if !requirement.matches(&version) {
        return Err(server_error(format!(
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
            Version::parse(candidate)
                .ok()
                .or_else(|| parse_calendar_version(candidate))
        })
        .next()
}

fn parse_calendar_version(candidate: &str) -> Option<Version> {
    let (date, time) = candidate.split_once('-')?;
    let mut date = date.split('.');
    let year = fixed_width_number(date.next()?, 4)?;
    let month = fixed_width_number(date.next()?, 2)?;
    let day = fixed_width_number(date.next()?, 2)?;
    if date.next().is_some() {
        return None;
    }

    let mut time = time.split('.');
    let hour = fixed_width_number(time.next()?, 2)?;
    let minute = fixed_width_number(time.next()?, 2)?;
    let second = fixed_width_number(time.next()?, 2)?;
    if time.next().is_some() || year == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > days_in_month {
        return None;
    }

    // ponytail: compatibility is day-granular; preserve time only if same-day releases diverge.
    Some(Version::new(year, month, day))
}

fn fixed_width_number(value: &str, width: usize) -> Option<u64> {
    (value.len() == width && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

async fn probe_npm_package(
    executable: &Path,
    package: &str,
    requirement: &str,
) -> Result<NpmProbe, ClspError> {
    if !executable.is_file() {
        return Err(server_error("executable candidate is not a file"));
    }
    for (manifest, modules_root) in npm_package_manifest_candidates(executable, package) {
        let Ok(bytes) = tokio::fs::read(&manifest).await else {
            continue;
        };
        let version_output = parse_npm_manifest_probe(&bytes, package, requirement)?;
        return Ok(NpmProbe {
            version_output,
            modules_root,
        });
    }
    Err(server_error(format!(
        "cannot locate package metadata for {package}"
    )))
}

async fn probe_npm_manifest_in_root(
    modules_root: &Path,
    package: &str,
    requirement: &str,
) -> Result<String, ClspError> {
    let manifest = modules_root.join(package).join("package.json");
    let bytes = tokio::fs::read(&manifest).await.map_err(|error| {
        server_error(format!(
            "cannot read npm manifest {}: {error}",
            manifest.display()
        ))
    })?;
    parse_npm_manifest_probe(&bytes, package, requirement)
}

fn parse_npm_manifest_probe(
    bytes: &[u8],
    package: &str,
    requirement: &str,
) -> Result<String, ClspError> {
    if bytes.len() > 1024 * 1024 {
        return Err(server_error("npm package manifest exceeds limit"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(server_error)?;
    if value.get("name").and_then(serde_json::Value::as_str) != Some(package) {
        return Err(server_error(format!(
            "npm package manifest name does not match {package}"
        )));
    }
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| server_error("npm package manifest has no version"))?;
    let parsed = validate_version_output(version, requirement)?;
    Ok(format!("{package} {parsed}"))
}

fn npm_package_manifest_candidates(executable: &Path, package: &str) -> Vec<(PathBuf, PathBuf)> {
    let mut executables = vec![executable.to_path_buf()];
    if let Ok(canonical) = std::fs::canonicalize(executable)
        && canonical != executable
    {
        executables.push(canonical);
    }
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();
    for executable in executables {
        for ancestor in executable.ancestors().skip(1).take(10) {
            let nested = ancestor.join("node_modules");
            if seen.insert(nested.clone()) {
                roots.push(nested);
            }
            if ancestor
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("node_modules"))
                && seen.insert(ancestor.to_path_buf())
            {
                roots.push(ancestor.to_path_buf());
            }
        }
    }
    roots
        .into_iter()
        .map(|root| (root.join(package).join("package.json"), root))
        .collect()
}

async fn verify_exact_npm_manifest(
    modules_root: &Path,
    package: &str,
    version: &str,
) -> Result<(), ClspError> {
    let manifest = modules_root.join(package).join("package.json");
    let bytes = tokio::fs::read(&manifest).await.map_err(|error| {
        server_error(format!(
            "cannot read installed npm manifest {}: {error}",
            manifest.display()
        ))
    })?;
    if bytes.len() > 1024 * 1024 {
        return Err(server_error("installed npm manifest exceeds limit"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(server_error)?;
    if value.get("name").and_then(serde_json::Value::as_str) != Some(package)
        || value.get("version").and_then(serde_json::Value::as_str) != Some(version)
    {
        return Err(server_error(format!(
            "installed npm manifest must be exactly {package}@{version}"
        )));
    }
    Ok(())
}

fn system_curl() -> Result<PathBuf, ClspError> {
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let curl = PathBuf::from(system_root).join("System32/curl.exe");
        if curl.is_file() {
            return Ok(curl);
        }
    }
    let program = if cfg!(windows) { "curl.exe" } else { "curl" };
    which::which(program).map_err(|_| {
        runtime_error(
            "Windows curl.exe is required for CLSP clangd self-install; install clangd locally or set lsp.clangd.executable",
        )
    })
}

async fn verify_file_sha256(path: &Path, expected: &str) -> Result<(), ClspError> {
    let mut file = tokio::fs::File::open(path).await.map_err(server_error)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await.map_err(server_error)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = hex::encode(digest.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(server_error(format!(
            "archive SHA-256 mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

fn extract_zip(
    archive_path: &Path,
    destination: &Path,
    expanded_limit: u64,
) -> Result<(), ClspError> {
    let file = std::fs::File::open(archive_path).map_err(server_error)?;
    let mut archive = zip::ZipArchive::new(file).map_err(server_error)?;
    if archive.len() > ARCHIVE_ENTRY_LIMIT {
        return Err(server_error("archive contains too many entries"));
    }
    std::fs::create_dir_all(destination).map_err(server_error)?;
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(server_error)?;
        if entry.is_symlink() {
            return Err(server_error("archive contains a symbolic link"));
        }
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| server_error("archive contains an unsafe path"))?;
        if !entry.is_dir() {
            expanded = expanded
                .checked_add(entry.size())
                .ok_or_else(|| server_error("archive expanded size overflow"))?;
            if expanded > expanded_limit {
                return Err(server_error("archive exceeds the expanded size limit"));
            }
        }
        let output = destination.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&output).map_err(server_error)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(server_error)?;
        }
        let mut output_file = std::fs::File::create(&output).map_err(server_error)?;
        let copied = std::io::copy(&mut entry, &mut output_file).map_err(server_error)?;
        if copied != entry.size() {
            return Err(server_error("archive entry size changed during extraction"));
        }
    }
    Ok(())
}

fn github_zip_candidate(
    artifacts: &Path,
    server_id: &str,
    version: &str,
    executable: &str,
) -> PathBuf {
    artifacts.join(server_id).join(version).join(executable)
}

fn vscode_clangd_candidates_from(app_data: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for product in ["Code", "Code - Insiders"] {
        let install_root = app_data
            .join(product)
            .join("User/globalStorage/llvm-vs-code-extensions.vscode-clangd/install");
        let Ok(entries) = std::fs::read_dir(install_root) else {
            continue;
        };
        for outer in entries
            .filter_map(Result::ok)
            .take(VSCODE_INSTALL_ENTRY_LIMIT)
        {
            let outer = outer.path();
            push_vscode_clangd_candidate(&outer, &mut candidates, &mut seen);
            let Ok(children) = std::fs::read_dir(&outer) else {
                continue;
            };
            for child in children
                .filter_map(Result::ok)
                .take(VSCODE_INSTALL_ENTRY_LIMIT)
            {
                push_vscode_clangd_candidate(&child.path(), &mut candidates, &mut seen);
            }
        }
    }
    candidates.sort_by(|(left_version, left_path), (right_version, right_path)| {
        right_version
            .cmp(left_version)
            .then_with(|| left_path.cmp(right_path))
    });
    candidates.into_iter().map(|(_, path)| path).collect()
}

fn push_vscode_clangd_candidate(
    directory: &Path,
    candidates: &mut Vec<(Version, PathBuf)>,
    seen: &mut BTreeSet<PathBuf>,
) {
    let Some(version) = directory
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("clangd_"))
        .and_then(|version| Version::parse(version).ok())
    else {
        return;
    };
    let executable = directory.join("bin/clangd.exe");
    if executable.is_file() && seen.insert(executable.clone()) {
        candidates.push((version, executable));
    }
}

fn local_candidates<'a>(
    server: &'a ServerDefinition,
    workspace: &'a Path,
    explicit: Option<&'a Path>,
) -> Vec<(ExecutableSource, PathBuf)> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let virtual_env_bin = if cfg!(windows) { "Scripts" } else { "bin" };
    for base in [
        workspace.join("node_modules").join(".bin"),
        workspace.join(".venv").join(virtual_env_bin),
        workspace.join("bin"),
    ] {
        for path in executable_candidates_in(&base, &server.command) {
            if seen.insert(path.clone()) {
                candidates.push((ExecutableSource::ProjectLocal, path));
            }
        }
    }
    if let Some(explicit) = explicit {
        let path = if explicit.is_absolute() {
            explicit.to_path_buf()
        } else {
            workspace.join(explicit)
        };
        if seen.insert(path.clone()) {
            candidates.push((ExecutableSource::Explicit, path));
        }
    }
    for name in executable_names(&server.command) {
        if let Ok(path) = which::which(&name)
            && seen.insert(path.clone())
        {
            candidates.push((ExecutableSource::Path, path));
        }
    }
    candidates
}

fn executable_candidates_in(directory: &Path, command: &str) -> Vec<PathBuf> {
    executable_names(command)
        .into_iter()
        .map(|name| directory.join(name))
        .collect()
}

fn executable_names(command: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{command}.exe"),
            format!("{command}.cmd"),
            command.to_owned(),
        ]
    } else {
        vec![command.to_owned()]
    }
}

fn dotnet_cli_home() -> Option<PathBuf> {
    ["DOTNET_CLI_HOME", "USERPROFILE", "HOME"]
        .into_iter()
        .find_map(std::env::var_os)
        .map(PathBuf::from)
}

fn dotnet_tool_candidates(home: &Path, command: &str) -> Vec<PathBuf> {
    executable_candidates_in(&home.join(".dotnet/tools"), command)
}

fn dotnet_tool_command_args(args: &[String], installed: bool) -> Result<Vec<String>, ClspError> {
    let mut args = args.to_vec();
    if installed {
        if args.first().map(String::as_str) != Some("tool")
            || args.get(1).map(String::as_str) != Some("install")
        {
            return Err(server_error("invalid dotnet tool install recipe"));
        }
        args[1] = "update".to_owned();
        args.push("--allow-downgrade".to_owned());
    }
    Ok(args)
}

fn parse_dotnet_tool_version(output: &[u8], package: &str) -> Result<Option<String>, ClspError> {
    let text = std::str::from_utf8(output).map_err(server_error)?;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some(package) {
            continue;
        }
        let version = fields
            .next()
            .ok_or_else(|| server_error("dotnet tool list returned a row without a version"))?;
        Version::parse(version).map_err(server_error)?;
        return Ok(Some(version.to_owned()));
    }
    Ok(None)
}

fn npm_install_args(
    manager: NpmManager,
    package: &str,
    version: &str,
    companions: &[String],
) -> Vec<String> {
    let mut args = manager.install_args();
    args.push(format!("{package}@{version}"));
    args.extend(companions.iter().cloned());
    args
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
    let mut candidates: Vec<_> = local_candidates(server, workspace, explicit)
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect();
    if let InstallRecipe::GithubZip {
        version,
        executable,
        ..
    } = &server.install
    {
        for name in ["APPDATA", "LOCALAPPDATA"] {
            digest.update(
                std::env::var_os(name)
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_bytes(),
            );
        }
        if server.id == "clangd"
            && let Some(app_data) = std::env::var_os("APPDATA")
        {
            candidates.extend(vscode_clangd_candidates_from(&PathBuf::from(app_data)));
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(github_zip_candidate(
                &PathBuf::from(local_app_data).join("clsp/artifacts"),
                &server.id,
                version,
                executable,
            ));
        }
    }
    if server.id == "csharp" {
        for name in ["DOTNET_CLI_HOME", "USERPROFILE", "HOME"] {
            digest.update(
                std::env::var_os(name)
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_bytes(),
            );
        }
        if let Some(home) = dotnet_cli_home() {
            candidates.extend(dotnet_tool_candidates(&home, &server.command));
        }
    }
    for candidate in candidates {
        hash_executable_candidate(&mut digest, candidate);
    }
    hex::encode(digest.finalize())
}

fn hash_executable_candidate(digest: &mut Sha256, candidate: PathBuf) {
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

pub(crate) fn sanitize_command(command: &mut Command) {
    let preserved: Vec<_> = PRESERVED_ENV
        .iter()
        .copied()
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

async fn run_command(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    duration: Duration,
) -> Result<CommandOutput, ClspError> {
    let mut command = Command::new(executable);
    command.args(args).current_dir(cwd);
    sanitize_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| server_error(format!("cannot start {}: {error}", executable.display())))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| server_error("child stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| server_error("child stderr was not captured"))?;
    let stdout = tokio::spawn(read_prefix(stdout));
    let stderr = tokio::spawn(read_prefix(stderr));

    let result = timeout(duration, child.wait()).await;
    let timed_out = result.is_err();
    let status = match result {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(server_error(format!(
                "cannot wait for {}: {error}",
                executable.display()
            )));
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            None
        }
    };
    let stdout = stdout.await.unwrap_or_default();
    let stderr = stderr.await.unwrap_or_default();
    if timed_out {
        return Err(server_error(format!(
            "{} timed out after {}s; stdout: {}; stderr: {}",
            executable.display(),
            duration.as_secs_f64(),
            bounded_text(&stdout),
            bounded_text(&stderr)
        )));
    }
    Ok(CommandOutput {
        status: status.expect("non-timeout child has an exit status"),
        stdout,
        stderr,
    })
}

async fn read_prefix(mut reader: impl AsyncRead + Unpin) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 1024];
    while let Ok(read) = reader.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let remaining = OUTPUT_LIMIT.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    kept
}

async fn run_checked(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    duration: Duration,
    label: &str,
) -> Result<CommandOutput, ClspError> {
    let output = run_command(executable, args, cwd, duration).await?;
    if !output.status.success() {
        return Err(server_error(format!(
            "{label} exited with {}; {}",
            output.status,
            command_output_detail(&output)
        )));
    }
    Ok(output)
}

fn command_output_detail(output: &CommandOutput) -> String {
    format!(
        "stdout: {}; stderr: {}",
        bounded_text(&output.stdout),
        bounded_text(&output.stderr)
    )
}

fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(OUTPUT_LIMIT)]).replace(['\r', '\n'], " ")
}

fn absolute_output_path(output: &[u8], label: &str) -> Result<PathBuf, ClspError> {
    let text = std::str::from_utf8(output).map_err(server_error)?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| server_error(format!("{label} returned no path")))?;
    let path = PathBuf::from(line);
    if !path.is_absolute() {
        return Err(server_error(format!("{label} is not absolute: {line}")));
    }
    Ok(path)
}

fn bun_modules_root(output: &[u8]) -> Result<PathBuf, ClspError> {
    let text = std::str::from_utf8(output).map_err(server_error)?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| server_error("bun pm ls --global returned no root"))?;
    let root = line
        .split_once(" node_modules (")
        .map(|(root, _)| PathBuf::from(root.trim()))
        .filter(|root| root.is_absolute())
        .ok_or_else(|| server_error("bun pm ls --global returned an unsupported root shape"))?;
    Ok(root.join("node_modules"))
}

fn go_bin_from_env_output(output: &[u8]) -> Result<Option<PathBuf>, ClspError> {
    let text = std::str::from_utf8(output).map_err(server_error)?;
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.split('\n');
    let gobin = lines.next().unwrap_or_default().trim();
    let gopath = lines.next().unwrap_or_default().trim();
    let bin = if gobin.is_empty() {
        std::env::split_paths(OsStr::new(gopath))
            .next()
            .map(|path| path.join("bin"))
    } else {
        Some(PathBuf::from(gobin))
    };
    match bin {
        Some(path) if path.is_absolute() => Ok(Some(path)),
        Some(path) => Err(server_error(format!(
            "go env reported a non-absolute bin path: {}",
            path.display()
        ))),
        None => Ok(None),
    }
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ClspError> {
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    tokio::fs::write(&temp, bytes).await.map_err(server_error)?;
    crate::ipc::atomic_replace(&temp, path).map_err(server_error)
}

fn runtime_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::RuntimeUnavailable, error.to_string())
}

fn server_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::ServerUnavailable, error.to_string()).retryable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use std::io::Write;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn test_resolver(root: &Path) -> ServerResolver {
        let paths = StatePaths {
            workspace_state: root.join("state"),
            logs: root.join("state/logs"),
            artifacts: root.join("artifacts"),
        };
        for path in [&paths.logs, &paths.artifacts] {
            std::fs::create_dir_all(path).unwrap();
        }
        let mut resolver = ServerResolver::new(Config::default(), paths);
        resolver.vscode_app_data = None;
        resolver.dotnet_cli_home = None;
        resolver
    }

    #[cfg(windows)]
    fn fake_executable(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = root.join(format!("{name}.cmd"));
        std::fs::write(&path, format!("@echo off\r\n{body}\r\n")).unwrap();
        path
    }

    #[cfg(windows)]
    fn compatible_test_executable(path: &Path) {
        std::fs::copy(system_curl().unwrap(), path).unwrap();
    }

    #[cfg(unix)]
    fn compatible_test_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, "#!/bin/sh\necho clangd version 22.1.6\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_test_zip(path: &Path, name: &str, bytes: &[u8]) {
        let file = std::fs::File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(bytes).unwrap();
        archive.finish().unwrap();
    }

    #[cfg(unix)]
    fn fake_executable(root: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn npm_manager_order_and_exact_argv_are_fixed() {
        assert_eq!(
            NpmManager::ALL,
            [NpmManager::Bun, NpmManager::Pnpm, NpmManager::Npm]
        );
        assert_eq!(
            NpmManager::Bun.install_args(),
            ["install", "--global", "--ignore-scripts"]
        );
        assert_eq!(
            NpmManager::Pnpm.install_args(),
            ["add", "--global", "--ignore-scripts"]
        );
        assert_eq!(
            NpmManager::Npm.install_args(),
            [
                "install",
                "--global",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund"
            ]
        );
        assert_eq!(
            npm_install_args(
                NpmManager::Bun,
                "@astrojs/language-server",
                "2.16.13",
                &["typescript@5.9.2".to_owned()]
            ),
            [
                "install",
                "--global",
                "--ignore-scripts",
                "@astrojs/language-server@2.16.13",
                "typescript@5.9.2"
            ]
        );
    }

    #[test]
    fn dotnet_tool_contract_is_exact() {
        let registry = Registry::builtin().unwrap();
        let csharp = registry.server("csharp").unwrap();
        let InstallRecipe::Command { args, .. } = &csharp.install else {
            panic!("C# must use a command recipe");
        };
        assert_eq!(dotnet_tool_command_args(args, false).unwrap(), *args);
        let mut update = args.clone();
        update[1] = "update".to_owned();
        update.push("--allow-downgrade".to_owned());
        assert_eq!(dotnet_tool_command_args(args, true).unwrap(), update);

        let list = b"Package Id Version Commands\n--------------------------------\nroslyn-language-server 5.9.0-1.26303.1 roslyn-language-server\n";
        assert_eq!(
            parse_dotnet_tool_version(list, ROSLYN_LANGUAGE_SERVER_PACKAGE).unwrap(),
            Some("5.9.0-1.26303.1".to_owned())
        );
        assert_eq!(
            parse_dotnet_tool_version(list, "other-package").unwrap(),
            None
        );
        assert!(
            parse_dotnet_tool_version(
                b"roslyn-language-server invalid",
                ROSLYN_LANGUAGE_SERVER_PACKAGE
            )
            .is_err()
        );
        assert!(PRESERVED_ENV.contains(&"DOTNET_CLI_HOME"));
        assert!(PRESERVED_ENV.contains(&"DOTNET_ROOT"));
        assert!(PRESERVED_ENV.contains(&"ProgramFiles(x86)"));
    }

    #[tokio::test]
    async fn dotnet_global_resolution_requires_manifest_and_compatible_shim() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("dotnet-home");
        let tools = home.join(".dotnet/tools");
        std::fs::create_dir_all(&tools).unwrap();
        let dotnet = fake_executable(
            root.path(),
            "dotnet",
            "echo Package Id Version Commands\necho roslyn-language-server 5.9.0-1.26303.1 roslyn-language-server",
        );
        let server_executable = fake_executable(
            &tools,
            ROSLYN_LANGUAGE_SERVER_PACKAGE,
            "echo roslyn-language-server 5.9.0-1.26303.1",
        );
        let mut resolver = test_resolver(root.path());
        resolver.dotnet_cli_home = Some(home);
        let server = Registry::builtin()
            .unwrap()
            .server("csharp")
            .unwrap()
            .clone();

        let resolution = resolver
            .resolve_toolchain_candidate(&server, root.path(), &dotnet, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolution.path, server_executable);
        assert_eq!(resolution.source, ExecutableSource::Path);

        #[cfg(windows)]
        std::fs::write(
            &dotnet,
            "@echo off\r\necho Package Id Version Commands\r\nexit /b 1\r\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::write(
            &dotnet,
            "#!/bin/sh\necho Package Id Version Commands\nexit 1\n",
        )
        .unwrap();
        assert_eq!(
            resolver
                .dotnet_global_tool_version(&dotnet, ROSLYN_LANGUAGE_SERVER_PACKAGE)
                .await
                .unwrap(),
            None
        );

        #[cfg(windows)]
        std::fs::write(
            &dotnet,
            "@echo off\r\necho roslyn-language-server 5.8.0 roslyn-language-server\r\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::write(
            &dotnet,
            "#!/bin/sh\necho roslyn-language-server 5.8.0 roslyn-language-server\n",
        )
        .unwrap();
        assert!(
            resolver
                .resolve_toolchain_candidate(&server, root.path(), &dotnet, false)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn manager_probe_skips_failures_without_reordering() {
        let root = tempfile::tempdir().unwrap();
        let resolver = test_resolver(root.path());
        let failed = fake_executable(root.path(), "bun", "exit /b 9");
        let working = fake_executable(root.path(), "pnpm", "echo 10.0.0");
        let unused = fake_executable(root.path(), "npm", "echo 12.0.0");

        let selected = resolver
            .select_npm_manager_from([
                (NpmManager::Bun, failed),
                (NpmManager::Pnpm, working.clone()),
                (NpmManager::Npm, unused),
            ])
            .await
            .unwrap();
        assert_eq!(selected.manager, NpmManager::Pnpm);
        assert_eq!(selected.executable, working);
        assert_eq!(
            resolver
                .select_npm_manager_from(Vec::new())
                .await
                .unwrap_err()
                .code,
            ErrorCode::RuntimeUnavailable
        );
    }

    #[tokio::test]
    async fn selected_manager_install_failure_is_terminal() {
        let root = tempfile::tempdir().unwrap();
        let resolver = test_resolver(root.path());
        let failed = fake_executable(root.path(), "bun", "exit /b 9");
        let server = Registry::builtin()
            .unwrap()
            .server("pyright")
            .unwrap()
            .clone();
        let manager = NpmManagerSelection {
            manager: NpmManager::Bun,
            executable: failed,
        };

        let error = resolver
            .install_npm(&server, &manager, "pyright", "1.1.405", &[])
            .await
            .unwrap_err();
        assert!(error.message.contains("bun global install"));
    }

    #[tokio::test]
    async fn post_install_missing_manager_roots_fail() {
        let root = tempfile::tempdir().unwrap();
        let resolver = test_resolver(root.path());
        let missing = root.path().join("missing-global-root");
        let manager = NpmManagerSelection {
            manager: NpmManager::Npm,
            executable: fake_executable(
                root.path(),
                "npm-roots",
                &format!("echo {}", missing.display()),
            ),
        };

        let error = match resolver.npm_roots(&manager, true).await {
            Ok(_) => panic!("missing roots must fail"),
            Err(error) => error,
        };
        assert!(error.message.contains("reported missing global roots"));
    }

    #[tokio::test]
    async fn compatible_project_executable_never_starts_install() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let bin = workspace.join("node_modules/.bin");
        let package = workspace.join("node_modules/pyright");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        let server = Registry::builtin()
            .unwrap()
            .server("pyright")
            .unwrap()
            .clone();
        std::fs::write(bin.join(&executable_names(&server.command)[0]), b"wrapper").unwrap();
        std::fs::write(
            package.join("package.json"),
            br#"{"name":"pyright","version":"1.1.405"}"#,
        )
        .unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let callback = Arc::clone(&called);

        let resolution = test_resolver(root.path())
            .resolve_server(&server, &workspace, None, move || async move {
                callback.store(true, Ordering::Relaxed);
            })
            .await
            .unwrap();
        assert_eq!(resolution.source, ExecutableSource::ProjectLocal);
        assert!(!called.load(Ordering::Relaxed));
    }

    #[test]
    fn vscode_clangd_candidates_are_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let older = root
            .path()
            .join("Code/User/globalStorage/llvm-vs-code-extensions.vscode-clangd/install/18.1.8/clangd_18.1.8/bin/clangd.exe");
        let newer = root
            .path()
            .join("Code - Insiders/User/globalStorage/llvm-vs-code-extensions.vscode-clangd/install/22.1.6/clangd_22.1.6/bin/clangd.exe");
        for path in [&older, &newer] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"candidate").unwrap();
        }

        assert_eq!(vscode_clangd_candidates_from(root.path()), [newer, older]);
    }

    #[tokio::test]
    async fn github_zip_resolution_prefers_vscode_then_cache() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let app_data = root.path().join("appdata");
        let extension = app_data
            .join("Code/User/globalStorage/llvm-vs-code-extensions.vscode-clangd/install/22.1.6/clangd_22.1.6/bin/clangd.exe");
        std::fs::create_dir_all(extension.parent().unwrap()).unwrap();
        compatible_test_executable(&extension);

        let mut resolver = test_resolver(root.path());
        resolver.vscode_app_data = Some(app_data);
        let mut server = Registry::builtin()
            .unwrap()
            .server("clangd")
            .unwrap()
            .clone();
        server.version_req = ">=1.0.0".into();
        let (version, executable) = match &server.install {
            InstallRecipe::GithubZip {
                version,
                executable,
                ..
            } => (version.clone(), executable.clone()),
            _ => unreachable!(),
        };
        let cached =
            github_zip_candidate(&resolver.paths.artifacts, &server.id, &version, &executable);
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        compatible_test_executable(&cached);
        let extension_candidates =
            vscode_clangd_candidates_from(resolver.vscode_app_data.as_deref().unwrap());
        assert_eq!(
            extension_candidates.as_slice(),
            std::slice::from_ref(&extension)
        );
        resolver
            .probe_server(&server, &extension, &workspace)
            .await
            .unwrap();

        let resolution = resolver
            .resolve_github_zip_existing(&server, &workspace, &version, &executable)
            .await
            .unwrap();
        assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
        assert_eq!(resolution.path, extension);

        std::fs::remove_file(&resolution.path).unwrap();
        let resolution = resolver
            .resolve_github_zip_existing(&server, &workspace, &version, &executable)
            .await
            .unwrap();
        assert_eq!(resolution.source, ExecutableSource::Installed);
        assert_eq!(resolution.path, cached);
    }

    #[tokio::test]
    async fn auto_install_false_does_not_create_github_zip_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let mut resolver = test_resolver(root.path());
        resolver.config.auto_install = false;
        let mut server = Registry::builtin()
            .unwrap()
            .server("clangd")
            .unwrap()
            .clone();
        server.command = "missing-clangd-for-auto-install-test".into();
        let called = Arc::new(AtomicBool::new(false));
        let callback = Arc::clone(&called);

        let error = resolver
            .resolve_server(&server, root.path(), None, move || async move {
                callback.store(true, Ordering::Relaxed);
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
        assert!(!called.load(Ordering::Relaxed));
        assert!(!resolver.paths.artifacts.join("clangd").exists());
    }

    #[tokio::test]
    async fn github_zip_checksum_and_extraction_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("clangd.zip");
        write_test_zip(&archive, "clangd/bin/clangd.exe", b"binary");
        let expected = hex::encode(Sha256::digest(std::fs::read(&archive).unwrap()));
        verify_file_sha256(&archive, &expected).await.unwrap();
        assert!(verify_file_sha256(&archive, &"0".repeat(64)).await.is_err());

        let destination = root.path().join("expanded");
        extract_zip(&archive, &destination, 6).unwrap();
        assert_eq!(
            std::fs::read(destination.join("clangd/bin/clangd.exe")).unwrap(),
            b"binary"
        );
        assert!(extract_zip(&archive, &root.path().join("too-large"), 5).is_err());

        let unsafe_archive = root.path().join("unsafe.zip");
        write_test_zip(&unsafe_archive, "../outside.exe", b"bad");
        assert!(extract_zip(&unsafe_archive, &root.path().join("unsafe"), 16).is_err());
    }

    #[tokio::test]
    async fn command_runner_bounds_output_and_reports_nonzero() {
        let root = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let body = "for /L %%i in (1,1,5000) do <nul set /p \"=x\"\r\n>&2 echo failed\r\nexit /b 7";
        #[cfg(unix)]
        let body = "head -c 5000 /dev/zero | tr '\\0' x\necho failed >&2\nexit 7";
        let executable = fake_executable(root.path(), "bounded", body);
        let output = run_command(&executable, &[], root.path(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout.len(), OUTPUT_LIMIT);
        assert!(bounded_text(&output.stderr).contains("failed"));
    }

    #[tokio::test]
    async fn command_runner_times_out_and_reaps() {
        let root = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let body = ":loop\r\ngoto loop";
        #[cfg(unix)]
        let body = "while :; do :; done";
        let executable = fake_executable(root.path(), "timeout", body);
        let error = run_command(&executable, &[], root.path(), Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(error.message.contains("timed out"));
    }

    #[test]
    fn locates_project_and_global_npm_package_manifests() {
        let project = Path::new("C:/work/node_modules/.bin/pyright-langserver.cmd");
        assert!(
            npm_package_manifest_candidates(project, "pyright").contains(&(
                PathBuf::from("C:/work/node_modules/pyright/package.json"),
                PathBuf::from("C:/work/node_modules")
            ))
        );

        let global = Path::new("C:/Users/me/AppData/Roaming/npm/pyright-langserver.cmd");
        assert!(
            npm_package_manifest_candidates(global, "pyright").contains(&(
                PathBuf::from("C:/Users/me/AppData/Roaming/npm/node_modules/pyright/package.json"),
                PathBuf::from("C:/Users/me/AppData/Roaming/npm/node_modules")
            ))
        );
    }

    #[tokio::test]
    async fn npm_server_version_comes_from_named_manifest_without_running_wrapper() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("node_modules/.bin");
        let package = root.path().join("node_modules/pyright");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        let executable = bin.join("pyright-langserver.cmd");
        std::fs::write(&executable, b"@exit /b 99").unwrap();
        std::fs::write(
            package.join("package.json"),
            br#"{"name":"pyright","version":"1.1.405"}"#,
        )
        .unwrap();

        let probe = probe_npm_package(&executable, "pyright", ">=1.1.300, <2.0.0")
            .await
            .unwrap();
        assert_eq!(probe.version_output, "pyright 1.1.405");
        assert_eq!(probe.modules_root, root.path().join("node_modules"));
    }

    #[tokio::test]
    async fn npm_global_resolution_uses_the_manager_modules_root() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("global-bin");
        let modules = root.path().join("global-store/node_modules");
        let package = modules.join("pyright");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        let server = Registry::builtin()
            .unwrap()
            .server("pyright")
            .unwrap()
            .clone();
        std::fs::write(bin.join(&executable_names(&server.command)[0]), b"wrapper").unwrap();
        std::fs::write(
            package.join("package.json"),
            br#"{"name":"pyright","version":"1.1.405"}"#,
        )
        .unwrap();

        let resolution = test_resolver(root.path())
            .resolve_npm_in_roots(
                &server,
                &NpmRoots {
                    bin,
                    modules: modules.clone(),
                },
                false,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolution.npm_modules_root, Some(modules));
        assert_eq!(resolution.version_output, "pyright 1.1.405");
    }

    #[tokio::test]
    async fn exact_npm_manifest_rejects_wrong_name_or_version() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("pyright");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            br#"{"name":"other","version":"1.1.405"}"#,
        )
        .unwrap();
        assert!(
            verify_exact_npm_manifest(root.path(), "pyright", "1.1.405")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn rustup_candidate_uses_reported_component_path() {
        let root = tempfile::tempdir().unwrap();
        let resolver = test_resolver(root.path());
        let analyzer = root.path().join(&executable_names("rust-analyzer")[0]);
        std::fs::write(&analyzer, b"binary").unwrap();
        let rustup = fake_executable(
            root.path(),
            "rustup",
            &format!("echo {}", analyzer.display()),
        );

        assert_eq!(
            resolver
                .rustup_candidate(&rustup, root.path(), true)
                .await
                .unwrap(),
            Some(analyzer)
        );
    }

    #[tokio::test]
    async fn manual_recipe_blocks_without_starting_install() {
        let root = tempfile::tempdir().unwrap();
        let mut server = Registry::builtin()
            .unwrap()
            .server("clangd")
            .unwrap()
            .clone();
        server.command = "missing-clangd-for-test".to_owned();
        server.install = InstallRecipe::Manual {
            version: "system".into(),
            hint: "install manually".into(),
        };
        let called = Arc::new(AtomicBool::new(false));
        let callback = Arc::clone(&called);

        let error = test_resolver(root.path())
            .resolve_server(&server, root.path(), None, move || async move {
                callback.store(true, Ordering::Relaxed);
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
        assert!(!called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn manual_clojure_reuses_an_explicit_compatible_server() {
        let root = tempfile::tempdir().unwrap();
        let executable = fake_executable(
            root.path(),
            "clojure-lsp",
            "echo clojure-lsp 2026.07.06-14.34.19",
        );
        let mut resolver = test_resolver(root.path());
        resolver.config.auto_install = false;
        let server = Registry::builtin()
            .unwrap()
            .server("clojure-lsp")
            .unwrap()
            .clone();
        let called = Arc::new(AtomicBool::new(false));
        let callback = Arc::clone(&called);

        let resolution = resolver
            .resolve_server(
                &server,
                root.path(),
                Some(&executable),
                move || async move {
                    callback.store(true, Ordering::Relaxed);
                },
            )
            .await
            .unwrap();
        assert_eq!(resolution.source, ExecutableSource::Explicit);
        assert_eq!(resolution.version_output, "clojure-lsp 2026.07.06-14.34.19");
        assert!(!called.load(Ordering::Relaxed));
    }

    #[test]
    fn parses_bun_and_go_roots() {
        let bun = if cfg!(windows) {
            br#"C:\Users\me\.bun\install\global node_modules (3)"#.as_slice()
        } else {
            b"/home/me/.bun/install/global node_modules (3)"
        };
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\Users\me\.bun\install\global\node_modules")
        } else {
            PathBuf::from("/home/me/.bun/install/global/node_modules")
        };
        assert_eq!(bun_modules_root(bun).unwrap(), expected);

        let go = if cfg!(windows) {
            b"\r\nC:\\Users\\me\\go\r\n".as_slice()
        } else {
            b"\n/home/me/go\n".as_slice()
        };
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\Users\me\go\bin")
        } else {
            PathBuf::from("/home/me/go/bin")
        };
        assert_eq!(go_bin_from_env_output(go).unwrap(), Some(expected));
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
            ("clojure-lsp 2026.07.06-14.34.19", Version::new(2026, 7, 6)),
        ] {
            assert_eq!(parse_version(output), Some(expected));
        }
        assert!(validate_version_output("tool v1.4.0", ">=1.0.0, <2.0.0").is_ok());
        assert!(validate_version_output("tool v2.0.0", ">=1.0.0, <2.0.0").is_err());
        assert!(
            validate_version_output("clojure-lsp 2026.07.06-14.34.19", ">=2026.7.6, <2027.0.0")
                .is_ok()
        );
        for invalid in [
            "2026.02.29-14.34.19",
            "2026.13.06-14.34.19",
            "2026.07.06-24.34.19",
            "2026.07.06-14.34",
        ] {
            assert_eq!(parse_version(invalid), None);
        }
        assert!(PRESERVED_ENV.contains(&"JAVA_HOME"));
    }

    #[test]
    fn executable_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let registry = Registry::builtin().unwrap();
        let server = registry.server("rust").unwrap();
        let executable = bin.join(&executable_names(&server.command)[0]);
        std::fs::write(&executable, b"one").unwrap();
        let first = resolution_fingerprint(server, directory.path(), None);
        std::fs::write(executable, b"different-size").unwrap();
        let second = resolution_fingerprint(server, directory.path(), None);
        assert_ne!(first, second);
    }
}

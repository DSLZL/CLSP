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
const VSCODE_EXTENSION_ENTRY_LIMIT: usize = 512;
const ROSLYN_LANGUAGE_SERVER_PACKAGE: &str = "roslyn-language-server";
const ELIXIR_LS_SERVER_ID: &str = "elixir-ls";
const ELIXIR_LS_VERSION_FILE_LIMIT: u64 = 128;
const ESLINT_SERVER_ID: &str = "eslint";
const FSHARP_SERVER_ID: &str = "fsharp";
const FSHARP_LANGUAGE_SERVER_PACKAGE: &str = "fsautocomplete";
const IONIDE_FSHARP_VERSION_REQ: &str = ">=7.31.1, <7.32.0";
const INTELEPHENSE_SERVER_ID: &str = "intelephense";
const INTELEPHENSE_EXTENSION_PREFIX: &str = "bmewburn.vscode-intelephense-client-";
const PRISMA_SERVER_ID: &str = "prisma";
const PRISMA_EXTENSION_PREFIX: &str = "prisma.prisma-";
const PYRIGHT_SERVER_ID: &str = "pyright";
const PYRIGHT_EXTENSION_PREFIX: &str = "ms-pyright.pyright-";
const JDTLS_SERVER_ID: &str = "jdtls";
const JDTLS_EXTENSION_PREFIX: &str = "redhat.java-";
const JDTLS_PLUGIN_ENTRY_LIMIT: usize = 512;
const JULIALS_SERVER_ID: &str = "julials";
const JULIALS_EXTENSION_PREFIX: &str = "julialang.language-julia-";
const JULIALS_LANGUAGE_SERVER_UUID: &str = "2b0e0bc5-e4fd-59b4-8912-456d1b03d8d7";
const JULIALS_FILE_LIMIT: u64 = 1024 * 1024;
const JULIALS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const JULIALS_PROBE_SCRIPT: &str = r#"using TOML; path = Base.find_package("LanguageServer"); path === nothing && exit(2); project = TOML.parsefile(joinpath(dirname(dirname(path)), "Project.toml")); println(VERSION); println(project["version"]); print(path)"#;
const KOTLIN_LS_SERVER_ID: &str = "kotlin-ls";
const KOTLIN_EXTENSION_PREFIX: &str = "jetbrains.kotlin-server-";
const KOTLIN_EXTENSION_VERSION_REQ: &str = ">=0.0.6, <0.1.0";
const KOTLIN_METADATA_FILE_LIMIT: u64 = 1024 * 1024;
const LUA_LS_SERVER_ID: &str = "lua-ls";
const LUA_EXTENSION_PREFIX: &str = "sumneko.lua-";
const LUA_EXTENSION_FILE_LIMIT: u64 = 1024 * 1024;
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
    "GRADLE_USER_HOME",
    "GOBIN",
    "GOPATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "DOTNET_CLI_HOME",
    "DOTNET_ROOT",
    "JULIA_DEPOT_PATH",
    "JULIA_LOAD_PATH",
    "JULIA_PROJECT",
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
    vscode_user_home: Option<PathBuf>,
    dotnet_cli_home: Option<PathBuf>,
    install_lock: Mutex<()>,
}

impl ServerResolver {
    pub fn new(config: Config, paths: StatePaths) -> Self {
        Self {
            config,
            paths,
            vscode_app_data: std::env::var_os("APPDATA").map(PathBuf::from),
            vscode_user_home: vscode_user_home(),
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
        if server.id == ELIXIR_LS_SERVER_ID {
            self.require_program("elixir", None)
                .await
                .map_err(|error| error.for_server(&server.id))?;
        }
        if matches!(
            server.id.as_str(),
            ESLINT_SERVER_ID | INTELEPHENSE_SERVER_ID | PRISMA_SERVER_ID | PYRIGHT_SERVER_ID
        ) {
            let requirement = match server.id.as_str() {
                PRISMA_SERVER_ID => Some(">=20.0.0"),
                PYRIGHT_SERVER_ID => Some(">=14.0.0"),
                _ => None,
            };
            self.require_program("node", requirement)
                .await
                .map_err(|error| error.for_server(&server.id))?;
        }
        if let Some(resolution) = self.resolve_local(server, workspace, explicit).await {
            return Ok(resolution);
        }

        if server.id == KOTLIN_LS_SERVER_ID {
            for candidate in path_kotlin_candidates() {
                if let Some(resolution) = self
                    .resolve_candidate(server, workspace, candidate, ExecutableSource::Path)
                    .await
                {
                    return Ok(resolution);
                }
            }
        }

        if server.id == ELIXIR_LS_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_elixir_ls(server, workspace).await
        {
            return Ok(resolution);
        }
        if server.id == ESLINT_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_eslint(server, workspace).await
        {
            return Ok(resolution);
        }
        if server.id == INTELEPHENSE_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_intelephense(server).await
        {
            return Ok(resolution);
        }
        if server.id == PRISMA_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_prisma(server).await
        {
            return Ok(resolution);
        }
        if server.id == PYRIGHT_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_pyright(server).await
        {
            return Ok(resolution);
        }
        if server.id == FSHARP_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_fsharp(server, workspace).await
        {
            return Ok(resolution);
        }
        if server.id == JDTLS_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_jdtls(server, workspace).await
        {
            return Ok(resolution);
        }
        if server.id == JULIALS_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_julials(server, workspace).await
        {
            return Ok(resolution);
        }
        if server.id == KOTLIN_LS_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_kotlin(server, workspace).await
        {
            return Ok(resolution);
        }
        if server.id == LUA_LS_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_lua(server, workspace).await
        {
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
                .require_program(program, None)
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
        if server.id == FSHARP_SERVER_ID
            && let Some(resolution) = self.resolve_vscode_fsharp(server, workspace).await
        {
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
                    .require_program(program, None)
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

    async fn resolve_vscode_elixir_ls(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_elixir_ls_candidates_from(home) {
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
        None
    }

    async fn resolve_vscode_eslint(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_eslint_candidates_from(home) {
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
        None
    }

    async fn resolve_vscode_intelephense(
        &self,
        server: &ServerDefinition,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_intelephense_candidates_from(home) {
            let Ok(probe) = validate_vscode_intelephense_extension(&candidate, &server.version_req)
            else {
                continue;
            };
            return Some(ResolvedExecutable {
                path: candidate,
                version_output: probe.version_output,
                source: ExecutableSource::VsCodeExtension,
                npm_modules_root: Some(probe.modules_root),
            });
        }
        None
    }

    async fn resolve_vscode_prisma(&self, server: &ServerDefinition) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_prisma_candidates_from(home) {
            let Ok(version_output) =
                validate_vscode_prisma_extension(&candidate, &server.version_req)
            else {
                continue;
            };
            return Some(ResolvedExecutable {
                path: candidate,
                version_output,
                source: ExecutableSource::VsCodeExtension,
                npm_modules_root: None,
            });
        }
        None
    }

    async fn resolve_vscode_pyright(
        &self,
        server: &ServerDefinition,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_pyright_candidates_from(home) {
            let Ok(version_output) =
                validate_vscode_pyright_extension(&candidate, &server.version_req)
            else {
                continue;
            };
            return Some(ResolvedExecutable {
                path: candidate,
                version_output,
                source: ExecutableSource::VsCodeExtension,
                npm_modules_root: None,
            });
        }
        None
    }

    async fn resolve_vscode_fsharp(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_fsharp_candidates_from(home) {
            if validate_vscode_fsharp_extension(&candidate).is_err() {
                continue;
            }
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
        None
    }

    async fn resolve_vscode_jdtls(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_jdtls_candidates_from(home) {
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
        None
    }

    async fn resolve_vscode_julials(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        let julia = path_julia_candidate()?;
        let probe_timeout =
            julials_probe_timeout(Duration::from_millis(self.config.runtime.probe_timeout_ms));
        let (julia_version, _) = probe_julia_version(&julia, workspace, probe_timeout, ">=1.11.0")
            .await
            .ok()?;
        for candidate in vscode_julials_candidates_from(home, &julia_version) {
            let Ok(version_output) = probe_vscode_julials(
                &julia,
                &candidate,
                workspace,
                &server.version_req,
                &julia_version,
                probe_timeout,
            )
            .await
            else {
                continue;
            };
            return Some(ResolvedExecutable {
                path: candidate,
                version_output,
                source: ExecutableSource::VsCodeExtension,
                npm_modules_root: None,
            });
        }
        None
    }

    async fn resolve_vscode_kotlin(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_kotlin_candidates_from(home) {
            if validate_vscode_kotlin_extension(&candidate, &server.version_req).is_err() {
                continue;
            }
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
        None
    }

    async fn resolve_vscode_lua(
        &self,
        server: &ServerDefinition,
        workspace: &Path,
    ) -> Option<ResolvedExecutable> {
        let home = self.vscode_user_home.as_deref()?;
        for candidate in vscode_lua_candidates_from(home) {
            let Ok((_, extension_version)) =
                validate_vscode_lua_extension(&candidate, &server.version_req)
            else {
                continue;
            };
            let Some(resolution) = self
                .resolve_candidate(
                    server,
                    workspace,
                    candidate,
                    ExecutableSource::VsCodeExtension,
                )
                .await
            else {
                continue;
            };
            if validate_lua_server_version(&resolution.version_output, &extension_version).is_ok() {
                return Some(resolution);
            }
        }
        None
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

    async fn require_program(
        &self,
        program: &str,
        requirement: Option<&str>,
    ) -> Result<PathBuf, ClspError> {
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
        if let Some(requirement) = requirement {
            validate_version_output(&command_output_detail(&output), requirement).map_err(
                |_| {
                    runtime_error(format!(
                        "{program} version does not satisfy {requirement}: {}",
                        command_output_detail(&output)
                    ))
                },
            )?;
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
            "csharp" | FSHARP_SERVER_ID => self.dotnet_tool_candidate(server, program).await?,
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
        let dotnet_tool = match (&server.install, dotnet_tool_package(&server.id)) {
            (InstallRecipe::Command { version, .. }, Some(package)) => {
                Some((package, version.as_str()))
            }
            _ => None,
        };
        let command_args = if let Some((package, _)) = dotnet_tool {
            let installed = self
                .dotnet_global_tool_version(program, package)
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

        if let Some((package, expected)) = dotnet_tool {
            let actual = self.dotnet_global_tool_version(program, package).await?;
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
        let Some(package) = dotnet_tool_package(&server.id) else {
            return Ok(None);
        };
        if self
            .dotnet_global_tool_version(dotnet, package)
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
        if server.id == ELIXIR_LS_SERVER_ID {
            return Ok(ServerProbe {
                version_output: probe_elixir_ls_release(executable, &server.version_req)?,
                npm_modules_root: None,
            });
        }
        if server.id == ESLINT_SERVER_ID {
            return probe_vscode_eslint_server(executable, working_dir, &server.version_req).await;
        }
        if server.id == JDTLS_SERVER_ID {
            let version_output = if executable
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
            {
                probe_vscode_jdtls(
                    executable,
                    working_dir,
                    &server.version_req,
                    Duration::from_millis(self.config.runtime.probe_timeout_ms),
                )
                .await?
            } else {
                probe_jdtls_launcher(
                    executable,
                    working_dir,
                    Duration::from_millis(self.config.runtime.probe_timeout_ms),
                )
                .await?
            };
            return Ok(ServerProbe {
                version_output,
                npm_modules_root: None,
            });
        }
        if server.id == JULIALS_SERVER_ID {
            return Ok(ServerProbe {
                version_output: probe_julials(
                    executable,
                    None,
                    working_dir,
                    &server.version_req,
                    ">=1.10.0",
                    julials_probe_timeout(Duration::from_millis(
                        self.config.runtime.probe_timeout_ms,
                    )),
                )
                .await?
                .version_output,
                npm_modules_root: None,
            });
        }
        if server.id == FSHARP_SERVER_ID
            && executable
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
        {
            if !executable.is_file() {
                return Err(server_error("FsAutoComplete DLL candidate is not a file"));
            }
            let dotnet = self.require_program("dotnet", None).await?;
            let mut args = vec![executable.to_string_lossy().into_owned()];
            args.extend(server.version_args.iter().cloned());
            let version_output = self
                .probe_compatible(&dotnet, &args, working_dir, &server.version_req)
                .await?;
            return Ok(ServerProbe {
                version_output,
                npm_modules_root: None,
            });
        }
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
            let candidate = candidate
                .strip_prefix("ILS-")
                .or_else(|| candidate.strip_prefix("LS-"))
                .unwrap_or(candidate);
            let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
            Version::parse(candidate)
                .ok()
                .or_else(|| parse_pvp_version(candidate))
                .or_else(|| parse_calendar_version(candidate))
        })
        .next()
}

fn parse_pvp_version(candidate: &str) -> Option<Version> {
    let parse_component = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse().ok())
            .flatten()
    };
    let mut components = candidate.split('.');
    let major = parse_component(components.next()?)?;
    let minor = parse_component(components.next()?)?;
    let patch = parse_component(components.next()?)?;
    let _revision: u64 = parse_component(components.next()?)?;
    if components.next().is_some() {
        return None;
    }

    // ponytail: SemVer has three numeric components; keep the PVP revision in the raw probe output.
    Some(Version::new(major, minor, patch))
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

fn probe_elixir_ls_release(executable: &Path, requirement: &str) -> Result<String, ClspError> {
    if !executable.is_file() {
        return Err(server_error("ElixirLS launcher candidate is not a file"));
    }
    let launcher = executable
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            ["language_server.bat", "language_server.sh"]
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
        });
    if !launcher {
        return Err(server_error(
            "ElixirLS candidate is not an official launcher",
        ));
    }
    let version_file = executable
        .parent()
        .ok_or_else(|| server_error("ElixirLS launcher has no release directory"))?
        .join("VERSION");
    let metadata = std::fs::metadata(&version_file)
        .map_err(|error| server_error(format!("cannot inspect ElixirLS VERSION: {error}")))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > ELIXIR_LS_VERSION_FILE_LIMIT {
        return Err(server_error(
            "ElixirLS VERSION is not a bounded regular file",
        ));
    }
    let version = std::fs::read_to_string(&version_file)
        .map_err(|error| server_error(format!("cannot read ElixirLS VERSION: {error}")))?;
    let version = version.trim();
    validate_version_output(version, requirement)?;
    Ok(version.to_owned())
}

fn vscode_elixir_ls_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    let launcher = if cfg!(windows) {
        "language_server.bat"
    } else {
        "language_server.sh"
    };
    vscode_extension_candidates_from(
        user_home,
        "jakebecker.elixir-ls-",
        Path::new("elixir-ls-release").join(launcher).as_path(),
    )
}

fn vscode_eslint_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    vscode_extension_candidates_from(
        user_home,
        "dbaeumer.vscode-eslint-",
        Path::new("server/out/eslintServer.js"),
    )
}

fn vscode_intelephense_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    vscode_extension_candidates_from(
        user_home,
        INTELEPHENSE_EXTENSION_PREFIX,
        Path::new("node_modules/intelephense/lib/intelephense.js"),
    )
}

fn vscode_prisma_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    vscode_extension_candidates_from(
        user_home,
        PRISMA_EXTENSION_PREFIX,
        Path::new("dist/language-server/bin.js"),
    )
}

fn vscode_pyright_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    vscode_extension_candidates_from(
        user_home,
        PYRIGHT_EXTENSION_PREFIX,
        Path::new("dist/server.js"),
    )
}

fn vscode_fsharp_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for (_, root) in vscode_extension_roots_from(user_home, "ionide.ionide-fsharp-") {
        let Ok(entries) = std::fs::read_dir(root.join("bin")) else {
            continue;
        };
        let mut frameworks: Vec<_> = entries
            .filter_map(Result::ok)
            .take(VSCODE_EXTENSION_ENTRY_LIMIT)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .and_then(OsStr::to_str)
                        .is_some_and(|name| name.starts_with("net"))
            })
            .collect();
        frameworks.sort();
        candidates.extend(frameworks.into_iter().filter_map(|framework| {
            let executable = framework.join("fsautocomplete.dll");
            executable.is_file().then_some(executable)
        }));
    }
    candidates
}

fn vscode_jdtls_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    vscode_extension_roots_from(user_home, JDTLS_EXTENSION_PREFIX)
        .into_iter()
        .filter_map(|(_, root)| jdtls_launcher_in_extension(&root).ok())
        .collect()
}

fn vscode_kotlin_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    let launcher = if cfg!(windows) {
        "server/bin/intellij-server.exe"
    } else {
        "server/bin/intellij-server"
    };
    vscode_extension_candidates_from(user_home, KOTLIN_EXTENSION_PREFIX, Path::new(launcher))
}

fn vscode_lua_candidates_from(user_home: &Path) -> Vec<PathBuf> {
    let launcher = if cfg!(windows) {
        "server/bin/lua-language-server.exe"
    } else {
        "server/bin/lua-language-server"
    };
    vscode_extension_candidates_from(user_home, LUA_EXTENSION_PREFIX, Path::new(launcher))
}

#[derive(Clone, Debug)]
pub(crate) struct JdtlsExtensionLayout {
    pub extension_root: PathBuf,
    pub configuration: PathBuf,
    pub core: PathBuf,
}

pub(crate) fn jdtls_extension_layout(launcher: &Path) -> Result<JdtlsExtensionLayout, ClspError> {
    if !launcher.is_file() {
        return Err(server_error("JDTLS launcher candidate is not a file"));
    }
    let plugins = launcher
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("plugins")))
        .ok_or_else(|| server_error("JDTLS launcher is not inside server/plugins"))?;
    let server_root = plugins
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("server")))
        .ok_or_else(|| server_error("JDTLS launcher has no server root"))?
        .to_path_buf();
    let extension_root = server_root
        .parent()
        .ok_or_else(|| server_error("JDTLS launcher has no extension root"))?
        .to_path_buf();
    let expected_launcher = single_jdtls_plugin(plugins, "org.eclipse.equinox.launcher_")?;
    let expected_launcher = std::fs::canonicalize(expected_launcher).map_err(server_error)?;
    let launcher = std::fs::canonicalize(launcher).map_err(server_error)?;
    if expected_launcher != launcher {
        return Err(server_error(
            "JDTLS launcher does not match the extension entry",
        ));
    }
    let core = single_jdtls_plugin(plugins, "org.eclipse.jdt.ls.core_")?;
    let configuration = server_root.join(if cfg!(windows) {
        "config_win"
    } else if cfg!(target_os = "macos") {
        "config_mac"
    } else {
        "config_linux"
    });
    if !configuration.is_dir() {
        return Err(server_error("JDTLS platform configuration is missing"));
    }
    Ok(JdtlsExtensionLayout {
        extension_root,
        configuration,
        core,
    })
}

fn jdtls_launcher_in_extension(extension_root: &Path) -> Result<PathBuf, ClspError> {
    single_jdtls_plugin(
        &extension_root.join("server/plugins"),
        "org.eclipse.equinox.launcher_",
    )
}

fn single_jdtls_plugin(directory: &Path, prefix: &str) -> Result<PathBuf, ClspError> {
    let entries = std::fs::read_dir(directory).map_err(server_error)?;
    let mut matches = entries
        .filter_map(Result::ok)
        .take(JDTLS_PLUGIN_ENTRY_LIMIT)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".jar"))
        });
    let plugin = matches
        .next()
        .ok_or_else(|| server_error(format!("JDTLS plugin {prefix}*.jar is missing")))?;
    if matches.next().is_some() {
        return Err(server_error(format!(
            "JDTLS plugin {prefix}*.jar is ambiguous"
        )));
    }
    Ok(plugin)
}

fn validate_vscode_jdtls_extension(
    launcher: &Path,
    requirement: &str,
) -> Result<(JdtlsExtensionLayout, Version), ClspError> {
    let layout = jdtls_extension_layout(launcher)?;
    let manifest = layout.extension_root.join("package.json");
    let metadata = std::fs::metadata(&manifest).map_err(server_error)?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(server_error(
            "redhat.java manifest is not a bounded regular file",
        ));
    }
    let bytes = std::fs::read(&manifest).map_err(server_error)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(server_error)?;
    if value.get("name").and_then(serde_json::Value::as_str) != Some("java")
        || value.get("publisher").and_then(serde_json::Value::as_str) != Some("redhat")
    {
        return Err(server_error(
            "JDTLS candidate is not the official redhat.java extension",
        ));
    }
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(|version| Version::parse(version).ok())
        .filter(|version| version.major == 1)
        .ok_or_else(|| server_error("redhat.java extension version is invalid"))?;
    let version = jdtls_core_version(&layout.core)?;
    let requirement = VersionReq::parse(requirement).map_err(server_error)?;
    if !requirement.matches(&version) {
        return Err(server_error(format!(
            "JDTLS core version {version} does not satisfy {requirement}"
        )));
    }
    Ok((layout, version))
}

fn jdtls_core_version(core: &Path) -> Result<Version, ClspError> {
    let name = core
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("org.eclipse.jdt.ls.core_"))
        .and_then(|name| name.strip_suffix(".jar"))
        .ok_or_else(|| server_error("JDTLS core plugin name is invalid"))?;
    parse_version(name).ok_or_else(|| server_error("JDTLS core plugin version is invalid"))
}

fn jdtls_java_candidates(extension_root: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push_directory = |directory: &Path| {
        for candidate in executable_candidates_in(directory, "java") {
            if candidate.is_file() && seen.insert(candidate.clone()) {
                candidates.push(candidate);
            }
        }
    };
    if let Some(extension_root) = extension_root {
        let jre = extension_root.join("jre");
        push_directory(&jre.join("bin"));
        let mut embedded = std::fs::read_dir(&jre)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .take(VSCODE_INSTALL_ENTRY_LIMIT)
            .map(|entry| entry.path().join("bin"))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        embedded.sort();
        for directory in embedded {
            push_directory(&directory);
        }
    }
    if let Some(java_home) = std::env::var_os("JAVA_HOME") {
        push_directory(&PathBuf::from(java_home).join("bin"));
    }
    for name in executable_names("java") {
        if let Ok(candidate) = which::which(name)
            && seen.insert(candidate.clone())
        {
            candidates.push(candidate);
        }
    }
    candidates
}

async fn resolve_java_21(
    extension_root: Option<&Path>,
    working_dir: &Path,
    probe_timeout: Duration,
) -> Result<(PathBuf, u64, String), ClspError> {
    for candidate in jdtls_java_candidates(extension_root) {
        let args = vec!["-version".to_owned()];
        let Ok(output) = run_checked(
            &candidate,
            &args,
            working_dir,
            probe_timeout,
            "Java runtime probe",
        )
        .await
        else {
            continue;
        };
        let text = format!(
            "{} {}",
            bounded_text(&output.stdout),
            bounded_text(&output.stderr)
        );
        if let Some(major) = java_major_version(&text).filter(|major| *major >= 21) {
            return Ok((candidate, major, text.trim().chars().take(512).collect()));
        }
    }
    Err(runtime_error(
        "Eclipse JDT Language Server requires Java 21+",
    ))
}

fn java_major_version(output: &str) -> Option<u64> {
    parse_version(output).map(|version| version.major)
}

pub(crate) async fn jdtls_java_for_launcher(
    launcher: &Path,
    working_dir: &Path,
    probe_timeout: Duration,
) -> Result<(PathBuf, u64), ClspError> {
    let layout = jdtls_extension_layout(launcher)?;
    resolve_java_21(Some(&layout.extension_root), working_dir, probe_timeout)
        .await
        .map(|(java, major, _)| (java, major))
}

async fn probe_vscode_jdtls(
    launcher: &Path,
    working_dir: &Path,
    requirement: &str,
    probe_timeout: Duration,
) -> Result<String, ClspError> {
    let (layout, version) = validate_vscode_jdtls_extension(launcher, requirement)?;
    let (_, _, java) =
        resolve_java_21(Some(&layout.extension_root), working_dir, probe_timeout).await?;
    Ok(format!("Eclipse JDT LS {version}; {java}"))
}

async fn probe_jdtls_launcher(
    executable: &Path,
    working_dir: &Path,
    probe_timeout: Duration,
) -> Result<String, ClspError> {
    if !executable.is_file() {
        return Err(server_error("JDTLS launcher candidate is not a file"));
    }
    let output = run_checked(
        executable,
        &["--help".to_owned()],
        working_dir,
        probe_timeout,
        "JDTLS launcher probe",
    )
    .await?;
    let help = format!(
        "{} {}",
        bounded_text(&output.stdout),
        bounded_text(&output.stderr)
    );
    let normalized = help.to_ascii_lowercase();
    if !normalized.contains("usage:") || !normalized.contains("jdtls") {
        return Err(server_error("JDTLS launcher help output is invalid"));
    }
    let (_, _, java) = resolve_java_21(None, working_dir, probe_timeout).await?;
    Ok(format!("jdtls launcher; {java}"))
}

#[derive(Clone, Debug)]
struct JuliaLsExtensionLayout {
    extension_root: PathBuf,
    extension_version: Version,
    environment: PathBuf,
    project: PathBuf,
    manifest: PathBuf,
    package_project: PathBuf,
}

struct JuliaLsProbe {
    julia_version: Version,
    server_version: Version,
    package_path: PathBuf,
    version_output: String,
}

fn path_julia_candidate() -> Option<PathBuf> {
    executable_names("julia")
        .into_iter()
        .find_map(|name| which::which(name).ok())
}

async fn probe_julia_version(
    executable: &Path,
    working_dir: &Path,
    probe_timeout: Duration,
    requirement: &str,
) -> Result<(Version, String), ClspError> {
    if !executable.is_file() {
        return Err(server_error("Julia candidate is not a file"));
    }
    let output = run_checked(
        executable,
        &["--version".to_owned()],
        working_dir,
        probe_timeout,
        "Julia runtime probe",
    )
    .await?;
    let text = if output.stdout.is_empty() {
        bounded_text(&output.stderr)
    } else {
        bounded_text(&output.stdout)
    };
    let version = validate_version_output(&text, requirement)?;
    Ok((version, text.trim().chars().take(512).collect()))
}

fn julials_probe_args(project: Option<&Path>) -> Vec<String> {
    let mut args = vec!["--startup-file=no".into(), "--history-file=no".into()];
    if let Some(project) = project {
        args.push(format!("--project={}", project.to_string_lossy()));
    }
    args.extend(["-e".into(), JULIALS_PROBE_SCRIPT.into()]);
    args
}

fn julials_probe_timeout(configured: Duration) -> Duration {
    configured.max(JULIALS_PROBE_TIMEOUT)
}

async fn probe_julials(
    executable: &Path,
    project: Option<&Path>,
    working_dir: &Path,
    server_requirement: &str,
    julia_requirement: &str,
    probe_timeout: Duration,
) -> Result<JuliaLsProbe, ClspError> {
    if !executable.is_file() {
        return Err(server_error("Julia candidate is not a file"));
    }
    let output = run_checked(
        executable,
        &julials_probe_args(project),
        working_dir,
        probe_timeout,
        "Julia LanguageServer probe",
    )
    .await?;
    parse_julials_probe_output(&output.stdout, server_requirement, julia_requirement)
}

fn parse_julials_probe_output(
    output: &[u8],
    server_requirement: &str,
    julia_requirement: &str,
) -> Result<JuliaLsProbe, ClspError> {
    let text = std::str::from_utf8(output).map_err(server_error)?;
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let julia = lines
        .next()
        .ok_or_else(|| server_error("Julia probe returned no runtime version"))?;
    let server = lines
        .next()
        .ok_or_else(|| server_error("Julia probe returned no LanguageServer version"))?;
    let package = lines
        .next()
        .ok_or_else(|| server_error("Julia probe returned no LanguageServer path"))?;
    if lines.next().is_some() {
        return Err(server_error("Julia probe returned unexpected output"));
    }
    let julia_version = validate_version_output(julia, julia_requirement)?;
    let server_version = validate_version_output(server, server_requirement)?;
    let package_path = PathBuf::from(package);
    if !package_path.is_absolute() || !package_path.is_file() {
        return Err(server_error(
            "Julia probe returned an invalid LanguageServer path",
        ));
    }
    Ok(JuliaLsProbe {
        julia_version: julia_version.clone(),
        server_version: server_version.clone(),
        package_path,
        version_output: format!("Julia {julia_version}; LanguageServer {server_version}"),
    })
}

fn vscode_julials_candidates_from(user_home: &Path, julia_version: &Version) -> Vec<PathBuf> {
    vscode_extension_roots_from(user_home, JULIALS_EXTENSION_PREFIX)
        .into_iter()
        .filter_map(|(_, root)| {
            let environments = root.join("scripts/environments/languageserver");
            let matching =
                environments.join(format!("v{}.{}", julia_version.major, julia_version.minor));
            let environment = if matching.is_dir() {
                matching
            } else {
                environments.join("fallback")
            };
            let project = environment.join("Project.toml");
            project.is_file().then_some(project)
        })
        .collect()
}

fn vscode_julials_environment_projects_from(user_home: &Path) -> Vec<PathBuf> {
    let mut projects = Vec::new();
    for (_, root) in vscode_extension_roots_from(user_home, JULIALS_EXTENSION_PREFIX) {
        let Ok(entries) = std::fs::read_dir(root.join("scripts/environments/languageserver"))
        else {
            continue;
        };
        let mut extension_projects = entries
            .filter_map(Result::ok)
            .take(VSCODE_INSTALL_ENTRY_LIMIT)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .and_then(OsStr::to_str)
                        .is_some_and(valid_julials_environment_name)
            })
            .map(|environment| environment.join("Project.toml"))
            .filter(|project| project.is_file())
            .collect::<Vec<_>>();
        extension_projects.sort();
        projects.extend(extension_projects);
    }
    projects
}

fn valid_julials_environment_name(name: &str) -> bool {
    if name == "fallback" {
        return true;
    }
    let Some((major, minor)) = name
        .strip_prefix('v')
        .and_then(|value| value.split_once('.'))
    else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && minor.bytes().all(|byte| byte.is_ascii_digit())
}

fn julials_extension_layout(project: &Path) -> Result<JuliaLsExtensionLayout, ClspError> {
    if !project.is_file() || project.file_name() != Some(OsStr::new("Project.toml")) {
        return Err(server_error(
            "Julia extension candidate is not an environment Project.toml",
        ));
    }
    let environment = project
        .parent()
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(valid_julials_environment_name)
        })
        .ok_or_else(|| server_error("Julia extension environment name is invalid"))?;
    let languageserver = environment
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("languageserver")))
        .ok_or_else(|| server_error("Julia extension environment is outside languageserver"))?;
    let environments = languageserver
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("environments")))
        .ok_or_else(|| server_error("Julia extension environment is outside environments"))?;
    let scripts = environments
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("scripts")))
        .ok_or_else(|| server_error("Julia extension environment is outside scripts"))?;
    let extension_root = scripts
        .parent()
        .ok_or_else(|| server_error("Julia extension environment has no extension root"))?;
    let extension_version = extension_root
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix(JULIALS_EXTENSION_PREFIX))
        .and_then(|version| Version::parse(version).ok())
        .filter(|version| version.major == 1)
        .ok_or_else(|| {
            server_error("Julia server is outside an official Julia extension directory")
        })?;
    let extension_root = std::fs::canonicalize(extension_root).map_err(server_error)?;
    let project = std::fs::canonicalize(project).map_err(server_error)?;
    let manifest =
        std::fs::canonicalize(environment.join("Manifest.toml")).map_err(server_error)?;
    let package_project =
        std::fs::canonicalize(extension_root.join("scripts/packages/LanguageServer/Project.toml"))
            .map_err(server_error)?;
    for path in [&project, &manifest, &package_project] {
        if !path.starts_with(&extension_root) {
            return Err(server_error(
                "Julia extension path escapes its extension root",
            ));
        }
    }
    Ok(JuliaLsExtensionLayout {
        extension_root,
        extension_version,
        environment: project.parent().unwrap().to_path_buf(),
        project,
        manifest,
        package_project,
    })
}

pub(crate) fn julials_extension_environment(project: &Path) -> Result<PathBuf, ClspError> {
    julials_extension_layout(project).map(|layout| layout.environment)
}

fn read_julials_file(path: &Path, label: &str) -> Result<Vec<u8>, ClspError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| server_error(format!("cannot inspect {label}: {error}")))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > JULIALS_FILE_LIMIT {
        return Err(server_error(format!(
            "{label} is not a bounded regular file"
        )));
    }
    std::fs::read(path).map_err(server_error)
}

fn read_julials_toml(path: &Path, label: &str) -> Result<toml::Value, ClspError> {
    let bytes = read_julials_file(path, label)?;
    let text = std::str::from_utf8(&bytes).map_err(server_error)?;
    toml::from_str(text).map_err(server_error)
}

fn validate_vscode_julials_environment(
    project: &Path,
    requirement: &str,
) -> Result<(JuliaLsExtensionLayout, Version, Version), ClspError> {
    let layout = julials_extension_layout(project)?;
    let extension_manifest = read_julials_file(
        &layout.extension_root.join("package.json"),
        "Julia extension manifest",
    )?;
    let extension_manifest: serde_json::Value =
        serde_json::from_slice(&extension_manifest).map_err(server_error)?;
    if extension_manifest
        .get("name")
        .and_then(serde_json::Value::as_str)
        != Some("language-julia")
        || extension_manifest
            .get("publisher")
            .and_then(serde_json::Value::as_str)
            != Some("julialang")
    {
        return Err(server_error(
            "Julia server is not from the official julialang.language-julia extension",
        ));
    }
    let extension_version = extension_manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(|version| Version::parse(version).ok())
        .filter(|version| version.major == 1)
        .ok_or_else(|| server_error("Julia extension version is invalid"))?;
    if extension_version != layout.extension_version {
        return Err(server_error(
            "Julia extension manifest version does not match its directory",
        ));
    }

    let environment_project = read_julials_toml(&layout.project, "Julia environment Project.toml")?;
    if environment_project
        .get("deps")
        .and_then(toml::Value::as_table)
        .and_then(|deps| deps.get("LanguageServer"))
        .and_then(toml::Value::as_str)
        != Some(JULIALS_LANGUAGE_SERVER_UUID)
    {
        return Err(server_error(
            "Julia extension environment does not declare official LanguageServer.jl",
        ));
    }

    let package_project = read_julials_toml(
        &layout.package_project,
        "Julia extension LanguageServer Project.toml",
    )?;
    if package_project.get("name").and_then(toml::Value::as_str) != Some("LanguageServer")
        || package_project.get("uuid").and_then(toml::Value::as_str)
            != Some(JULIALS_LANGUAGE_SERVER_UUID)
    {
        return Err(server_error(
            "Julia extension package is not official LanguageServer.jl",
        ));
    }
    let server_version = package_project
        .get("version")
        .and_then(toml::Value::as_str)
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Julia extension LanguageServer version is invalid"))?;
    if !VersionReq::parse(requirement)
        .map_err(server_error)?
        .matches(&server_version)
    {
        return Err(server_error(format!(
            "Julia extension LanguageServer version {server_version} does not satisfy {requirement}"
        )));
    }

    let environment_manifest =
        read_julials_toml(&layout.manifest, "Julia environment Manifest.toml")?;
    let entries = environment_manifest
        .get("deps")
        .and_then(toml::Value::as_table)
        .and_then(|deps| deps.get("LanguageServer"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| server_error("Julia extension manifest has no LanguageServer entry"))?;
    if entries.len() != 1 {
        return Err(server_error(
            "Julia extension manifest has an ambiguous LanguageServer entry",
        ));
    }
    let entry = entries[0]
        .as_table()
        .ok_or_else(|| server_error("Julia extension LanguageServer entry is invalid"))?;
    let manifest_version = entry
        .get("version")
        .and_then(toml::Value::as_str)
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Julia extension manifest version is invalid"))?;
    if entry.get("uuid").and_then(toml::Value::as_str) != Some(JULIALS_LANGUAGE_SERVER_UUID)
        || manifest_version != server_version
    {
        return Err(server_error(
            "Julia extension manifest does not match LanguageServer.jl",
        ));
    }
    let package_path = entry
        .get("path")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| server_error("Julia extension manifest has no LanguageServer path"))?;
    let package_path =
        std::fs::canonicalize(layout.environment.join(package_path)).map_err(server_error)?;
    let expected_package = layout
        .package_project
        .parent()
        .ok_or_else(|| server_error("Julia extension package has no root"))?;
    if package_path != expected_package {
        return Err(server_error(
            "Julia extension manifest resolves LanguageServer outside the official package",
        ));
    }
    Ok((layout, extension_version, server_version))
}

async fn probe_vscode_julials(
    julia: &Path,
    project: &Path,
    working_dir: &Path,
    requirement: &str,
    expected_julia: &Version,
    probe_timeout: Duration,
) -> Result<String, ClspError> {
    let (layout, extension_version, server_version) =
        validate_vscode_julials_environment(project, requirement)?;
    let probe = probe_julials(
        julia,
        Some(&layout.environment),
        working_dir,
        requirement,
        ">=1.11.0",
        probe_timeout,
    )
    .await?;
    if &probe.julia_version != expected_julia || probe.server_version != server_version {
        return Err(server_error(
            "Julia extension probe does not match the selected environment",
        ));
    }
    let package = std::fs::canonicalize(&probe.package_path).map_err(server_error)?;
    let package_root = layout
        .package_project
        .parent()
        .ok_or_else(|| server_error("Julia extension package has no root"))?;
    if !package.starts_with(package_root) {
        return Err(server_error(
            "Julia extension resolved LanguageServer outside the extension",
        ));
    }
    Ok(format!(
        "{}; julialang.language-julia {extension_version}",
        probe.version_output
    ))
}

fn vscode_extension_candidates_from(
    user_home: &Path,
    extension_prefix: &str,
    relative_executable: &Path,
) -> Vec<PathBuf> {
    vscode_extension_roots_from(user_home, extension_prefix)
        .into_iter()
        .filter_map(|(_, root)| {
            let executable = root.join(relative_executable);
            executable.is_file().then_some(executable)
        })
        .collect()
}

fn vscode_extension_roots_from(
    user_home: &Path,
    extension_prefix: &str,
) -> Vec<(Version, PathBuf)> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for root in [
        user_home.join(".vscode/extensions"),
        user_home.join(".vscode-insiders/extensions"),
    ] {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries
            .filter_map(Result::ok)
            .take(VSCODE_EXTENSION_ENTRY_LIMIT)
        {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            let Some(prefix) = name.get(..extension_prefix.len()) else {
                continue;
            };
            if !prefix.eq_ignore_ascii_case(extension_prefix) {
                continue;
            }
            let Some(version) = name
                .get(extension_prefix.len()..)
                .and_then(|version| Version::parse(version).ok())
            else {
                continue;
            };
            if path.is_dir() && seen.insert(path.clone()) {
                candidates.push((version, path));
            }
        }
    }
    candidates.sort_by(|(left_version, left_path), (right_version, right_path)| {
        right_version
            .cmp(left_version)
            .then_with(|| left_path.cmp(right_path))
    });
    candidates
}

#[derive(Clone, Debug)]
struct LuaExtensionLayout {
    extension_root: PathBuf,
    manifest: PathBuf,
    server_main: PathBuf,
    bin_main: PathBuf,
    script: PathBuf,
    meta: PathBuf,
    locale: PathBuf,
}

fn lua_extension_layout(executable: &Path) -> Result<LuaExtensionLayout, ClspError> {
    let launcher = if cfg!(windows) {
        "lua-language-server.exe"
    } else {
        "lua-language-server"
    };
    if !executable.is_file()
        || !executable
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(launcher))
    {
        return Err(server_error(
            "LuaLS candidate is not the official platform launcher",
        ));
    }
    let bin = executable
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("bin")))
        .ok_or_else(|| server_error("LuaLS launcher is outside server/bin"))?;
    let server = bin
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("server")))
        .ok_or_else(|| server_error("LuaLS launcher is outside server/bin"))?;
    let extension_root = server
        .parent()
        .ok_or_else(|| server_error("LuaLS launcher has no extension root"))?;
    let extension_root = std::fs::canonicalize(extension_root).map_err(server_error)?;
    let executable = std::fs::canonicalize(executable).map_err(server_error)?;
    let manifest =
        std::fs::canonicalize(extension_root.join("package.json")).map_err(server_error)?;
    let server_main =
        std::fs::canonicalize(extension_root.join("server/main.lua")).map_err(server_error)?;
    let bin_main =
        std::fs::canonicalize(extension_root.join("server/bin/main.lua")).map_err(server_error)?;
    let script =
        std::fs::canonicalize(extension_root.join("server/script")).map_err(server_error)?;
    let meta = std::fs::canonicalize(extension_root.join("server/meta")).map_err(server_error)?;
    let locale =
        std::fs::canonicalize(extension_root.join("server/locale")).map_err(server_error)?;

    for path in [&executable, &manifest, &server_main, &bin_main] {
        if !path.starts_with(&extension_root) || !path.is_file() {
            return Err(server_error("LuaLS file escapes its extension root"));
        }
    }
    for path in [&script, &meta, &locale] {
        if !path.starts_with(&extension_root) || !path.is_dir() {
            return Err(server_error("LuaLS directory escapes its extension root"));
        }
    }

    Ok(LuaExtensionLayout {
        extension_root,
        manifest,
        server_main,
        bin_main,
        script,
        meta,
        locale,
    })
}

fn validate_vscode_lua_extension(
    executable: &Path,
    requirement: &str,
) -> Result<(LuaExtensionLayout, Version), ClspError> {
    let layout = lua_extension_layout(executable)?;
    let directory_name = layout
        .extension_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| server_error("Lua extension root has no name"))?;
    let directory_version = directory_name
        .get(LUA_EXTENSION_PREFIX.len()..)
        .filter(|_| {
            directory_name
                .get(..LUA_EXTENSION_PREFIX.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(LUA_EXTENSION_PREFIX))
        })
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("LuaLS is outside an official extension root"))?;

    let metadata = std::fs::metadata(&layout.manifest).map_err(server_error)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > LUA_EXTENSION_FILE_LIMIT {
        return Err(server_error(
            "Lua extension manifest is not a bounded regular file",
        ));
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&layout.manifest).map_err(server_error)?)
            .map_err(server_error)?;
    if manifest.get("name").and_then(serde_json::Value::as_str) != Some("lua")
        || manifest
            .get("publisher")
            .and_then(serde_json::Value::as_str)
            != Some("sumneko")
    {
        return Err(server_error(
            "LuaLS is not from the official sumneko.lua extension",
        ));
    }
    let extension_version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Lua extension manifest version is invalid"))?;
    if (
        extension_version.major,
        extension_version.minor,
        extension_version.patch,
    ) != (
        directory_version.major,
        directory_version.minor,
        directory_version.patch,
    ) {
        return Err(server_error(
            "Lua extension manifest version does not match its directory",
        ));
    }
    validate_version_output(&extension_version.to_string(), requirement)?;
    Ok((layout, extension_version))
}

fn validate_lua_server_version(output: &str, expected: &Version) -> Result<(), ClspError> {
    let actual = parse_version(output)
        .ok_or_else(|| server_error("LuaLS probe returned no semantic version"))?;
    if &actual != expected {
        return Err(server_error(format!(
            "LuaLS server version {actual} does not match extension {expected}"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct KotlinExtensionLayout {
    extension_root: PathBuf,
    manifest: PathBuf,
    product_info: PathBuf,
    build_file: PathBuf,
    bundled_java: PathBuf,
}

fn kotlin_extension_layout(executable: &Path) -> Result<KotlinExtensionLayout, ClspError> {
    let launcher = if cfg!(windows) {
        "intellij-server.exe"
    } else {
        "intellij-server"
    };
    if !executable.is_file()
        || !executable
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(launcher))
    {
        return Err(server_error(
            "Kotlin candidate is not the official intellij-server launcher",
        ));
    }
    let bin = executable
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("bin")))
        .ok_or_else(|| server_error("Kotlin launcher is outside server/bin"))?;
    let server_root = bin
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("server")))
        .ok_or_else(|| server_error("Kotlin launcher is outside server/bin"))?;
    let extension_root = server_root
        .parent()
        .ok_or_else(|| server_error("Kotlin launcher has no extension root"))?;
    let extension_root = std::fs::canonicalize(extension_root).map_err(server_error)?;
    let executable = std::fs::canonicalize(executable).map_err(server_error)?;
    let manifest =
        std::fs::canonicalize(extension_root.join("package.json")).map_err(server_error)?;
    let product_info = std::fs::canonicalize(extension_root.join("server/product-info.json"))
        .map_err(server_error)?;
    let build_file =
        std::fs::canonicalize(extension_root.join("server/build.txt")).map_err(server_error)?;
    let bundled_java = std::fs::canonicalize(extension_root.join(if cfg!(windows) {
        "server/jbr/bin/java.exe"
    } else {
        "server/jbr/bin/java"
    }))
    .map_err(server_error)?;
    for path in [
        &executable,
        &manifest,
        &product_info,
        &build_file,
        &bundled_java,
    ] {
        if !path.starts_with(&extension_root) || !path.is_file() {
            return Err(server_error(
                "Kotlin extension path escapes its extension root",
            ));
        }
    }
    Ok(KotlinExtensionLayout {
        extension_root,
        manifest,
        product_info,
        build_file,
        bundled_java,
    })
}

fn read_kotlin_json(path: &Path, label: &str) -> Result<serde_json::Value, ClspError> {
    let metadata = std::fs::metadata(path).map_err(server_error)?;
    if !metadata.is_file() || metadata.len() > KOTLIN_METADATA_FILE_LIMIT {
        return Err(server_error(format!(
            "Kotlin {label} is not a bounded regular file"
        )));
    }
    let bytes = std::fs::read(path).map_err(server_error)?;
    serde_json::from_slice(&bytes).map_err(server_error)
}

fn validate_vscode_kotlin_extension(
    executable: &Path,
    requirement: &str,
) -> Result<KotlinExtensionLayout, ClspError> {
    let layout = kotlin_extension_layout(executable)?;
    let directory_name = layout
        .extension_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| server_error("Kotlin extension root has no name"))?;
    let directory_version = directory_name
        .get(KOTLIN_EXTENSION_PREFIX.len()..)
        .filter(|_| {
            directory_name
                .get(..KOTLIN_EXTENSION_PREFIX.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(KOTLIN_EXTENSION_PREFIX))
        })
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Kotlin server is outside an official extension root"))?;

    let manifest = read_kotlin_json(&layout.manifest, "manifest")?;
    if manifest.get("name").and_then(serde_json::Value::as_str) != Some("kotlin-server")
        || manifest
            .get("publisher")
            .and_then(serde_json::Value::as_str)
            != Some("JetBrains")
    {
        return Err(server_error(
            "Kotlin server is not from the official JetBrains.kotlin-server extension",
        ));
    }
    let extension_version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Kotlin extension manifest version is invalid"))?;
    if (
        extension_version.major,
        extension_version.minor,
        extension_version.patch,
    ) != (
        directory_version.major,
        directory_version.minor,
        directory_version.patch,
    ) {
        return Err(server_error(
            "Kotlin extension manifest version does not match its directory",
        ));
    }
    validate_version_output(&extension_version.to_string(), KOTLIN_EXTENSION_VERSION_REQ)?;

    let product = read_kotlin_json(&layout.product_info, "product-info.json")?;
    if product.get("name").and_then(serde_json::Value::as_str) != Some("kotlin-server")
        || product
            .get("productVendor")
            .and_then(serde_json::Value::as_str)
            != Some("JetBrains")
        || product
            .get("productCode")
            .and_then(serde_json::Value::as_str)
            != Some("ILS")
        || product
            .get("minRequiredJavaVersion")
            .and_then(serde_json::Value::as_u64)
            != Some(25)
    {
        return Err(server_error("Kotlin product metadata is invalid"));
    }
    let build = product
        .get("buildNumber")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| server_error("Kotlin product metadata has no build number"))?;
    validate_version_output(build, requirement)?;
    let expected_launcher = if cfg!(windows) {
        "bin/intellij-server.exe"
    } else {
        "bin/intellij-server"
    };
    let expected_java = if cfg!(windows) {
        "jbr/bin/java.exe"
    } else {
        "jbr/bin/java"
    };
    let launch_matches = product
        .get("launch")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|launches| {
            launches.iter().any(|launch| {
                launch
                    .get("launcherPath")
                    .and_then(serde_json::Value::as_str)
                    == Some(expected_launcher)
                    && launch
                        .get("javaExecutablePath")
                        .and_then(serde_json::Value::as_str)
                        == Some(expected_java)
                    && launch
                        .get("stdioRedirectArg")
                        .and_then(serde_json::Value::as_str)
                        == Some("--stdio")
            })
        });
    if !launch_matches {
        return Err(server_error("Kotlin product launch metadata is invalid"));
    }
    let build_metadata = std::fs::metadata(&layout.build_file).map_err(server_error)?;
    if build_metadata.len() > KOTLIN_METADATA_FILE_LIMIT {
        return Err(server_error("Kotlin build metadata exceeds limit"));
    }
    let build_text = std::fs::read_to_string(&layout.build_file).map_err(server_error)?;
    if build_text.trim() != format!("ILS-{build}") {
        return Err(server_error(
            "Kotlin build metadata does not match product info",
        ));
    }
    Ok(layout)
}

fn vscode_fsharp_extension_root(executable: &Path) -> Result<PathBuf, ClspError> {
    let name_is = |path: &Path, expected: &str| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    };
    if !executable.is_file() || !name_is(executable, "fsautocomplete.dll") {
        return Err(server_error(
            "F# candidate is not the official fsautocomplete.dll entry",
        ));
    }
    let framework = executable
        .parent()
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("net"))
        })
        .ok_or_else(|| server_error("FsAutoComplete entry is outside bin/net*"))?;
    let bin = framework
        .parent()
        .filter(|path| name_is(path, "bin"))
        .ok_or_else(|| server_error("FsAutoComplete entry is outside bin/net*"))?;
    bin.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| server_error("FsAutoComplete entry has no extension root"))
}

fn validate_vscode_fsharp_extension(executable: &Path) -> Result<(), ClspError> {
    let extension_root = vscode_fsharp_extension_root(executable)?;
    for sibling in [
        "fsautocomplete.deps.json",
        "fsautocomplete.runtimeconfig.json",
    ] {
        if !executable.with_file_name(sibling).is_file() {
            return Err(server_error(format!(
                "Ionide FsAutoComplete entry is missing {sibling}"
            )));
        }
    }

    let directory_name = extension_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| server_error("Ionide extension root has no name"))?;
    let prefix = "ionide.ionide-fsharp-";
    let directory_version = directory_name
        .get(prefix.len()..)
        .filter(|_| {
            directory_name
                .get(..prefix.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
        })
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("F# server is outside an official Ionide extension root"))?;

    let manifest = extension_root.join("package.json");
    let metadata = std::fs::metadata(&manifest).map_err(|error| {
        server_error(format!(
            "cannot inspect Ionide F# manifest {}: {error}",
            manifest.display()
        ))
    })?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(server_error(
            "Ionide F# manifest is not a bounded regular file",
        ));
    }
    let bytes = std::fs::read(&manifest).map_err(server_error)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(server_error)?;
    if value.get("name").and_then(serde_json::Value::as_str) != Some("Ionide-fsharp")
        || value.get("publisher").and_then(serde_json::Value::as_str) != Some("Ionide")
    {
        return Err(server_error(
            "F# server is not from the official Ionide.Ionide-fsharp extension",
        ));
    }
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| server_error("Ionide F# manifest has no version"))?;
    if Version::parse(version).map_err(server_error)? != directory_version {
        return Err(server_error(
            "Ionide F# manifest version does not match its extension directory",
        ));
    }
    validate_version_output(version, IONIDE_FSHARP_VERSION_REQ)?;
    Ok(())
}

fn vscode_intelephense_extension_root(executable: &Path) -> Result<PathBuf, ClspError> {
    let name_is = |path: &Path, expected: &str| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    };
    if !executable.is_file() || !name_is(executable, "intelephense.js") {
        return Err(server_error(
            "Intelephense candidate is not the official intelephense.js entry",
        ));
    }
    let lib = executable
        .parent()
        .filter(|path| name_is(path, "lib"))
        .ok_or_else(|| server_error("Intelephense entry is outside intelephense/lib"))?;
    let package = lib
        .parent()
        .filter(|path| name_is(path, "intelephense"))
        .ok_or_else(|| server_error("Intelephense entry is outside intelephense/lib"))?;
    let node_modules = package
        .parent()
        .filter(|path| name_is(path, "node_modules"))
        .ok_or_else(|| server_error("Intelephense entry is outside node_modules"))?;
    let extension_root = node_modules
        .parent()
        .ok_or_else(|| server_error("Intelephense entry has no extension root"))?;
    let extension_root = std::fs::canonicalize(extension_root).map_err(server_error)?;
    let executable = std::fs::canonicalize(executable).map_err(server_error)?;
    let expected =
        std::fs::canonicalize(extension_root.join("node_modules/intelephense/lib/intelephense.js"))
            .map_err(server_error)?;
    if executable != expected || !executable.starts_with(&extension_root) {
        return Err(server_error(
            "Intelephense entry escapes its extension root",
        ));
    }
    Ok(extension_root)
}

fn read_bounded_manifest(path: &Path, label: &str) -> Result<Vec<u8>, ClspError> {
    let metadata = std::fs::metadata(path).map_err(server_error)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
        return Err(server_error(format!(
            "{label} is not a bounded regular file"
        )));
    }
    std::fs::read(path).map_err(server_error)
}

fn validate_vscode_intelephense_extension(
    executable: &Path,
    requirement: &str,
) -> Result<NpmProbe, ClspError> {
    let extension_root = vscode_intelephense_extension_root(executable)?;
    let directory_name = extension_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| server_error("Intelephense extension root has no name"))?;
    let directory_version = directory_name
        .get(INTELEPHENSE_EXTENSION_PREFIX.len()..)
        .filter(|_| {
            directory_name
                .get(..INTELEPHENSE_EXTENSION_PREFIX.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(INTELEPHENSE_EXTENSION_PREFIX))
        })
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Intelephense is outside an official extension root"))?;

    let extension_manifest =
        std::fs::canonicalize(extension_root.join("package.json")).map_err(server_error)?;
    let server_manifest =
        std::fs::canonicalize(extension_root.join("node_modules/intelephense/package.json"))
            .map_err(server_error)?;
    for manifest in [&extension_manifest, &server_manifest] {
        if !manifest.starts_with(&extension_root) || !manifest.is_file() {
            return Err(server_error(
                "Intelephense manifest escapes its extension root",
            ));
        }
    }

    let extension_bytes =
        read_bounded_manifest(&extension_manifest, "Intelephense extension manifest")?;
    let extension: serde_json::Value =
        serde_json::from_slice(&extension_bytes).map_err(server_error)?;
    if extension.get("name").and_then(serde_json::Value::as_str)
        != Some("vscode-intelephense-client")
        || extension
            .get("publisher")
            .and_then(serde_json::Value::as_str)
            != Some("bmewburn")
    {
        return Err(server_error(
            "Intelephense is not from the official bmewburn.vscode-intelephense-client extension",
        ));
    }
    let extension_version = extension
        .get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Intelephense extension manifest version is invalid"))?;
    if extension_version != directory_version {
        return Err(server_error(
            "Intelephense extension manifest version does not match its directory",
        ));
    }

    let server_bytes = read_bounded_manifest(&server_manifest, "Intelephense server manifest")?;
    let version_output = parse_npm_manifest_probe(&server_bytes, "intelephense", requirement)?;
    if parse_version(&version_output).as_ref() != Some(&extension_version) {
        return Err(server_error(
            "Intelephense server version does not match its extension",
        ));
    }
    let modules_root = server_manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| server_error("Intelephense server has no node_modules root"))?
        .to_path_buf();
    Ok(NpmProbe {
        version_output,
        modules_root,
    })
}

fn vscode_prisma_extension_root(executable: &Path) -> Result<PathBuf, ClspError> {
    let name_is = |path: &Path, expected: &str| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    };
    if !executable.is_file() || !name_is(executable, "bin.js") {
        return Err(server_error(
            "Prisma candidate is not the official language-server/bin.js entry",
        ));
    }
    let language_server = executable
        .parent()
        .filter(|path| name_is(path, "language-server"))
        .ok_or_else(|| server_error("Prisma entry is outside dist/language-server"))?;
    let dist = language_server
        .parent()
        .filter(|path| name_is(path, "dist"))
        .ok_or_else(|| server_error("Prisma entry is outside dist/language-server"))?;
    let extension_root = dist
        .parent()
        .ok_or_else(|| server_error("Prisma entry has no extension root"))?;
    let extension_root = std::fs::canonicalize(extension_root).map_err(server_error)?;
    let executable = std::fs::canonicalize(executable).map_err(server_error)?;
    let expected = std::fs::canonicalize(extension_root.join("dist/language-server/bin.js"))
        .map_err(server_error)?;
    if executable != expected || !executable.starts_with(&extension_root) {
        return Err(server_error("Prisma entry escapes its extension root"));
    }
    Ok(extension_root)
}

fn validate_vscode_prisma_extension(
    executable: &Path,
    requirement: &str,
) -> Result<String, ClspError> {
    let extension_root = vscode_prisma_extension_root(executable)?;
    let directory_name = extension_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| server_error("Prisma extension root has no name"))?;
    let directory_version = directory_name
        .get(PRISMA_EXTENSION_PREFIX.len()..)
        .filter(|_| {
            directory_name
                .get(..PRISMA_EXTENSION_PREFIX.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PRISMA_EXTENSION_PREFIX))
        })
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Prisma is outside an official extension root"))?;

    let manifest =
        std::fs::canonicalize(extension_root.join("package.json")).map_err(server_error)?;
    let wasm = std::fs::canonicalize(
        extension_root.join("dist/language-server/prisma_schema_build_bg.wasm"),
    )
    .map_err(server_error)?;
    if !manifest.starts_with(&extension_root) || !wasm.starts_with(&extension_root) {
        return Err(server_error("Prisma extension files escape their root"));
    }
    let wasm_metadata = std::fs::metadata(&wasm).map_err(server_error)?;
    if !wasm_metadata.is_file() || wasm_metadata.len() == 0 {
        return Err(server_error("Prisma schema WASM is not a regular file"));
    }

    let bytes = read_bounded_manifest(&manifest, "Prisma extension manifest")?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(server_error)?;
    if value.get("name").and_then(serde_json::Value::as_str) != Some("prisma")
        || value.get("publisher").and_then(serde_json::Value::as_str) != Some("Prisma")
    {
        return Err(server_error(
            "Prisma server is not from the official Prisma.prisma extension",
        ));
    }
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| server_error("Prisma extension manifest has no version"))?;
    let manifest_version = Version::parse(version).map_err(server_error)?;
    if manifest_version != directory_version {
        return Err(server_error(
            "Prisma extension manifest version does not match its directory",
        ));
    }
    validate_version_output(version, requirement)?;
    Ok(format!("@prisma/language-server {version}"))
}

fn vscode_pyright_extension_root(executable: &Path) -> Result<PathBuf, ClspError> {
    let name_is = |path: &Path, expected: &str| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    };
    if !executable.is_file() || !name_is(executable, "server.js") {
        return Err(server_error(
            "Pyright candidate is not the official dist/server.js entry",
        ));
    }
    let dist = executable
        .parent()
        .filter(|path| name_is(path, "dist"))
        .ok_or_else(|| server_error("Pyright entry is outside dist"))?;
    let extension_root = dist
        .parent()
        .ok_or_else(|| server_error("Pyright entry has no extension root"))?;
    let extension_root = std::fs::canonicalize(extension_root).map_err(server_error)?;
    let executable = std::fs::canonicalize(executable).map_err(server_error)?;
    let expected =
        std::fs::canonicalize(extension_root.join("dist/server.js")).map_err(server_error)?;
    if executable != expected || !executable.starts_with(&extension_root) {
        return Err(server_error("Pyright entry escapes its extension root"));
    }
    Ok(extension_root)
}

fn validate_vscode_pyright_extension(
    executable: &Path,
    requirement: &str,
) -> Result<String, ClspError> {
    let extension_root = vscode_pyright_extension_root(executable)?;
    let directory_name = extension_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| server_error("Pyright extension root has no name"))?;
    let directory_version = directory_name
        .get(PYRIGHT_EXTENSION_PREFIX.len()..)
        .filter(|_| {
            directory_name
                .get(..PYRIGHT_EXTENSION_PREFIX.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PYRIGHT_EXTENSION_PREFIX))
        })
        .and_then(|version| Version::parse(version).ok())
        .ok_or_else(|| server_error("Pyright is outside an official extension root"))?;

    let manifest =
        std::fs::canonicalize(extension_root.join("package.json")).map_err(server_error)?;
    if !manifest.starts_with(&extension_root) {
        return Err(server_error("Pyright extension manifest escapes its root"));
    }
    let bytes = read_bounded_manifest(&manifest, "Pyright extension manifest")?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(server_error)?;
    if value.get("name").and_then(serde_json::Value::as_str) != Some("pyright")
        || value.get("publisher").and_then(serde_json::Value::as_str) != Some("ms-pyright")
    {
        return Err(server_error(
            "Pyright server is not from the official ms-pyright.pyright extension",
        ));
    }
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| server_error("Pyright extension manifest has no version"))?;
    let manifest_version = Version::parse(version).map_err(server_error)?;
    if manifest_version != directory_version {
        return Err(server_error(
            "Pyright extension manifest version does not match its directory",
        ));
    }
    validate_version_output(version, requirement)?;
    Ok(format!("pyright {version}"))
}

fn vscode_eslint_extension_root(executable: &Path) -> Result<PathBuf, ClspError> {
    let name_is = |path: &Path, expected: &str| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    };
    if !executable.is_file() || !name_is(executable, "eslintServer.js") {
        return Err(server_error(
            "ESLint candidate is not the official eslintServer.js entry",
        ));
    }
    let out = executable
        .parent()
        .filter(|path| name_is(path, "out"))
        .ok_or_else(|| server_error("ESLint server entry is outside server/out"))?;
    let server = out
        .parent()
        .filter(|path| name_is(path, "server"))
        .ok_or_else(|| server_error("ESLint server entry is outside server/out"))?;
    server
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| server_error("ESLint server entry has no extension root"))
}

async fn probe_vscode_eslint_server(
    executable: &Path,
    working_dir: &Path,
    requirement: &str,
) -> Result<ServerProbe, ClspError> {
    let extension_root = vscode_eslint_extension_root(executable)?;
    let manifest = extension_root.join("package.json");
    let bytes = tokio::fs::read(&manifest).await.map_err(|error| {
        server_error(format!(
            "cannot read VS Code ESLint manifest {}: {error}",
            manifest.display()
        ))
    })?;
    if bytes.len() > 1024 * 1024 {
        return Err(server_error("VS Code ESLint manifest exceeds limit"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(server_error)?;
    if value.get("name").and_then(serde_json::Value::as_str) != Some("vscode-eslint")
        || value.get("publisher").and_then(serde_json::Value::as_str) != Some("dbaeumer")
    {
        return Err(server_error(
            "ESLint server is not from the official dbaeumer.vscode-eslint extension",
        ));
    }
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| server_error("VS Code ESLint manifest has no version"))?;
    validate_version_output(version, requirement)?;

    let modules_root = working_dir.join("node_modules");
    probe_npm_manifest_in_root(&modules_root, "eslint", ">=1.0.0").await?;
    Ok(ServerProbe {
        version_output: format!("vscode-eslint {version}"),
        npm_modules_root: Some(modules_root),
    })
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

fn path_kotlin_candidates() -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    executable_names("intellij-server")
        .into_iter()
        .filter_map(|name| which::which(name).ok())
        .filter(|path| seen.insert(path.clone()))
        .collect()
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
            format!("{command}.bat"),
            command.to_owned(),
        ]
    } else {
        vec![command.to_owned()]
    }
}

fn vscode_user_home() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"]
        .into_iter()
        .find_map(std::env::var_os)
        .map(PathBuf::from)
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

fn dotnet_tool_package(server_id: &str) -> Option<&'static str> {
    match server_id {
        "csharp" => Some(ROSLYN_LANGUAGE_SERVER_PACKAGE),
        FSHARP_SERVER_ID => Some(FSHARP_LANGUAGE_SERVER_PACKAGE),
        _ => None,
    }
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
    if matches!(server.id.as_str(), "csharp" | FSHARP_SERVER_ID) {
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
    if matches!(
        server.id.as_str(),
        ELIXIR_LS_SERVER_ID
            | ESLINT_SERVER_ID
            | FSHARP_SERVER_ID
            | INTELEPHENSE_SERVER_ID
            | PRISMA_SERVER_ID
            | PYRIGHT_SERVER_ID
            | JDTLS_SERVER_ID
            | JULIALS_SERVER_ID
            | KOTLIN_LS_SERVER_ID
            | LUA_LS_SERVER_ID
    ) {
        for name in ["USERPROFILE", "HOME"] {
            digest.update(
                std::env::var_os(name)
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_bytes(),
            );
        }
        if let Some(home) = vscode_user_home() {
            match server.id.as_str() {
                ELIXIR_LS_SERVER_ID => candidates.extend(vscode_elixir_ls_candidates_from(&home)),
                ESLINT_SERVER_ID => candidates.extend(vscode_eslint_candidates_from(&home)),
                FSHARP_SERVER_ID => candidates.extend(vscode_fsharp_candidates_from(&home)),
                INTELEPHENSE_SERVER_ID => {
                    candidates.extend(vscode_intelephense_candidates_from(&home))
                }
                PRISMA_SERVER_ID => candidates.extend(vscode_prisma_candidates_from(&home)),
                PYRIGHT_SERVER_ID => candidates.extend(vscode_pyright_candidates_from(&home)),
                JDTLS_SERVER_ID => candidates.extend(vscode_jdtls_candidates_from(&home)),
                JULIALS_SERVER_ID => {
                    candidates.extend(vscode_julials_environment_projects_from(&home))
                }
                KOTLIN_LS_SERVER_ID => candidates.extend(vscode_kotlin_candidates_from(&home)),
                LUA_LS_SERVER_ID => candidates.extend(vscode_lua_candidates_from(&home)),
                _ => {}
            }
        }
    }
    if server.id == KOTLIN_LS_SERVER_ID {
        candidates.extend(path_kotlin_candidates());
    }
    if matches!(server.id.as_str(), JDTLS_SERVER_ID | KOTLIN_LS_SERVER_ID) {
        digest.update(
            std::env::var_os("JAVA_HOME")
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
    }
    if server.id == JULIALS_SERVER_ID {
        for name in ["JULIA_DEPOT_PATH", "JULIA_LOAD_PATH", "JULIA_PROJECT"] {
            digest.update(
                std::env::var_os(name)
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_bytes(),
            );
        }
    }
    if server.id == ESLINT_SERVER_ID {
        hash_executable_candidate(
            &mut digest,
            workspace.join("node_modules/eslint/package.json"),
        );
    }
    for candidate in candidates {
        let version_file = (server.id == ELIXIR_LS_SERVER_ID)
            .then(|| candidate.parent().map(|parent| parent.join("VERSION")))
            .flatten();
        hash_executable_candidate(&mut digest, candidate.clone());
        if let Some(version_file) = version_file {
            hash_executable_candidate(&mut digest, version_file);
        }
        if server.id == ESLINT_SERVER_ID
            && let Ok(extension_root) = vscode_eslint_extension_root(&candidate)
        {
            hash_executable_candidate(&mut digest, extension_root.join("package.json"));
        }
        if server.id == FSHARP_SERVER_ID
            && let Ok(extension_root) = vscode_fsharp_extension_root(&candidate)
        {
            hash_executable_candidate(&mut digest, extension_root.join("package.json"));
            hash_executable_candidate(
                &mut digest,
                candidate.with_file_name("fsautocomplete.deps.json"),
            );
            hash_executable_candidate(
                &mut digest,
                candidate.with_file_name("fsautocomplete.runtimeconfig.json"),
            );
        }
        if server.id == INTELEPHENSE_SERVER_ID
            && let Ok(extension_root) = vscode_intelephense_extension_root(&candidate)
        {
            hash_executable_candidate(&mut digest, extension_root.join("package.json"));
            hash_executable_candidate(
                &mut digest,
                extension_root.join("node_modules/intelephense/package.json"),
            );
        }
        if server.id == PRISMA_SERVER_ID
            && let Ok(extension_root) = vscode_prisma_extension_root(&candidate)
        {
            hash_executable_candidate(&mut digest, extension_root.join("package.json"));
            hash_executable_candidate(
                &mut digest,
                extension_root.join("dist/language-server/prisma_schema_build_bg.wasm"),
            );
        }
        if server.id == PYRIGHT_SERVER_ID
            && let Ok(extension_root) = vscode_pyright_extension_root(&candidate)
        {
            hash_executable_candidate(&mut digest, extension_root.join("package.json"));
        }
        if server.id == JDTLS_SERVER_ID
            && let Ok(layout) = jdtls_extension_layout(&candidate)
        {
            hash_executable_candidate(&mut digest, layout.extension_root.join("package.json"));
            hash_executable_candidate(&mut digest, layout.core);
            hash_executable_candidate(&mut digest, layout.configuration.join("config.ini"));
            for java in jdtls_java_candidates(Some(&layout.extension_root)) {
                hash_executable_candidate(&mut digest, java);
            }
        }
        if server.id == JULIALS_SERVER_ID
            && let Ok(layout) = julials_extension_layout(&candidate)
        {
            hash_executable_candidate(&mut digest, layout.extension_root.join("package.json"));
            hash_executable_candidate(&mut digest, layout.manifest);
            hash_executable_candidate(&mut digest, layout.package_project);
        }
        if server.id == KOTLIN_LS_SERVER_ID
            && let Ok(layout) = kotlin_extension_layout(&candidate)
        {
            hash_executable_candidate(&mut digest, layout.manifest);
            hash_executable_candidate(&mut digest, layout.product_info);
            hash_executable_candidate(&mut digest, layout.build_file);
            hash_executable_candidate(&mut digest, layout.bundled_java);
        }
        if server.id == LUA_LS_SERVER_ID
            && let Ok(layout) = lua_extension_layout(&candidate)
        {
            hash_executable_candidate(&mut digest, layout.manifest);
            hash_executable_candidate(&mut digest, layout.server_main);
            hash_executable_candidate(&mut digest, layout.bin_main);
            hash_executable_candidate(&mut digest, layout.script);
            hash_executable_candidate(&mut digest, layout.meta);
            hash_executable_candidate(&mut digest, layout.locale);
        }
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
        resolver.vscode_user_home = None;
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

    fn write_elixir_ls_release(extension_root: &Path, version: &str) -> PathBuf {
        let launcher = if cfg!(windows) {
            "language_server.bat"
        } else {
            "language_server.sh"
        };
        let release = extension_root
            .join(format!("jakebecker.elixir-ls-{version}"))
            .join("elixir-ls-release");
        std::fs::create_dir_all(&release).unwrap();
        let executable = release.join(launcher);
        std::fs::write(&executable, b"launcher").unwrap();
        std::fs::write(release.join("VERSION"), version).unwrap();
        executable
    }

    fn write_eslint_extension(extension_root: &Path, version: &str) -> PathBuf {
        let root = extension_root.join(format!("dbaeumer.vscode-eslint-{version}"));
        let server = root.join("server/out/eslintServer.js");
        std::fs::create_dir_all(server.parent().unwrap()).unwrap();
        std::fs::write(&server, b"server").unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "vscode-eslint",
                "publisher": "dbaeumer",
                "version": version,
            }))
            .unwrap(),
        )
        .unwrap();
        server
    }

    fn write_eslint_dependency(workspace: &Path, version: &str) {
        let package = workspace.join("node_modules/eslint");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            serde_json::to_vec(&serde_json::json!({"name": "eslint", "version": version})).unwrap(),
        )
        .unwrap();
    }

    fn write_intelephense_extension(extension_root: &Path, version: &str) -> PathBuf {
        let root = extension_root.join(format!("{INTELEPHENSE_EXTENSION_PREFIX}{version}"));
        let server = root.join("node_modules/intelephense/lib/intelephense.js");
        std::fs::create_dir_all(server.parent().unwrap()).unwrap();
        std::fs::write(&server, b"server").unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "vscode-intelephense-client",
                "publisher": "bmewburn",
                "version": version,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("node_modules/intelephense/package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "intelephense",
                "version": version,
            }))
            .unwrap(),
        )
        .unwrap();
        server
    }

    fn write_prisma_extension(extension_root: &Path, version: &str) -> PathBuf {
        let root = extension_root.join(format!("{PRISMA_EXTENSION_PREFIX}{version}"));
        let server = root.join("dist/language-server/bin.js");
        std::fs::create_dir_all(server.parent().unwrap()).unwrap();
        std::fs::write(&server, b"server").unwrap();
        std::fs::write(
            root.join("dist/language-server/prisma_schema_build_bg.wasm"),
            b"wasm",
        )
        .unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "prisma",
                "publisher": "Prisma",
                "version": version,
            }))
            .unwrap(),
        )
        .unwrap();
        server
    }

    fn write_pyright_extension(extension_root: &Path, version: &str) -> PathBuf {
        let root = extension_root.join(format!("{PYRIGHT_EXTENSION_PREFIX}{version}"));
        let server = root.join("dist/server.js");
        std::fs::create_dir_all(server.parent().unwrap()).unwrap();
        std::fs::write(&server, b"server").unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "pyright",
                "publisher": "ms-pyright",
                "version": version,
            }))
            .unwrap(),
        )
        .unwrap();
        server
    }

    fn write_fsharp_extension(
        extension_root: &Path,
        extension_version: &str,
        framework: &str,
    ) -> PathBuf {
        let root = extension_root.join(format!("ionide.ionide-fsharp-{extension_version}"));
        let server = root.join("bin").join(framework).join("fsautocomplete.dll");
        std::fs::create_dir_all(server.parent().unwrap()).unwrap();
        for name in [
            "fsautocomplete.dll",
            "fsautocomplete.deps.json",
            "fsautocomplete.runtimeconfig.json",
        ] {
            std::fs::write(server.with_file_name(name), b"server").unwrap();
        }
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "Ionide-fsharp",
                "publisher": "Ionide",
                "version": extension_version,
            }))
            .unwrap(),
        )
        .unwrap();
        server
    }

    fn write_kotlin_extension(
        extension_root: &Path,
        directory_version: &str,
        manifest_version: &str,
        server_version: &str,
    ) -> PathBuf {
        let root = extension_root.join(format!("{KOTLIN_EXTENSION_PREFIX}{directory_version}"));
        let launcher_path = if cfg!(windows) {
            "bin/intellij-server.exe"
        } else {
            "bin/intellij-server"
        };
        let java_path = if cfg!(windows) {
            "jbr/bin/java.exe"
        } else {
            "jbr/bin/java"
        };
        let server = root.join("server");
        let launcher = server.join(launcher_path);
        let java = server.join(java_path);
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::create_dir_all(java.parent().unwrap()).unwrap();
        std::fs::write(&launcher, b"launcher").unwrap();
        std::fs::write(&java, b"java").unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "kotlin-server",
                "publisher": "JetBrains",
                "version": manifest_version,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(server.join("build.txt"), format!("ILS-{server_version}\n")).unwrap();
        std::fs::write(
            server.join("product-info.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "kotlin-server",
                "buildNumber": server_version,
                "productCode": "ILS",
                "productVendor": "JetBrains",
                "minRequiredJavaVersion": 25,
                "launch": [{
                    "launcherPath": launcher_path,
                    "javaExecutablePath": java_path,
                    "stdioRedirectArg": "--stdio",
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        launcher
    }

    fn write_lua_extension(
        extension_root: &Path,
        directory_version: &str,
        manifest_version: &str,
    ) -> PathBuf {
        let root = extension_root.join(format!("{LUA_EXTENSION_PREFIX}{directory_version}"));
        let executable = root.join(if cfg!(windows) {
            "server/bin/lua-language-server.exe"
        } else {
            "server/bin/lua-language-server"
        });
        for directory in [
            executable.parent().unwrap().to_path_buf(),
            root.join("server/script"),
            root.join("server/meta"),
            root.join("server/locale"),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(&executable, b"launcher").unwrap();
        std::fs::write(root.join("server/main.lua"), b"return true").unwrap();
        std::fs::write(root.join("server/bin/main.lua"), b"return true").unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "lua",
                "publisher": "sumneko",
                "version": manifest_version,
            }))
            .unwrap(),
        )
        .unwrap();
        executable
    }

    fn write_jdtls_extension(
        extension_root: &Path,
        directory_version: &str,
        manifest_version: &str,
        core_version: &str,
        java_version: &str,
    ) -> (PathBuf, PathBuf) {
        let root = extension_root.join(format!("redhat.java-{directory_version}"));
        let plugins = root.join("server/plugins");
        let configuration = root.join("server").join(if cfg!(windows) {
            "config_win"
        } else if cfg!(target_os = "macos") {
            "config_mac"
        } else {
            "config_linux"
        });
        let java_bin = root.join("jre/bin");
        for directory in [&plugins, &configuration, &java_bin] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let launcher = plugins.join("org.eclipse.equinox.launcher_1.7.0.jar");
        std::fs::write(&launcher, b"launcher").unwrap();
        std::fs::write(
            plugins.join(format!("org.eclipse.jdt.ls.core_{core_version}.jar")),
            b"core",
        )
        .unwrap();
        std::fs::write(configuration.join("config.ini"), b"config").unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "java",
                "publisher": "redhat",
                "version": manifest_version,
            }))
            .unwrap(),
        )
        .unwrap();
        let java = fake_executable(
            &java_bin,
            "java",
            &format!("echo openjdk version \"{java_version}\" 1>&2"),
        );
        (launcher, java)
    }

    fn write_julials_extension(
        extension_root: &Path,
        extension_version: &str,
        environment_name: &str,
        server_version: &str,
    ) -> (PathBuf, PathBuf) {
        let root = extension_root.join(format!("{JULIALS_EXTENSION_PREFIX}{extension_version}"));
        let environment = root
            .join("scripts/environments/languageserver")
            .join(environment_name);
        let package = root.join("scripts/packages/LanguageServer");
        let source = package.join("src/LanguageServer.jl");
        std::fs::create_dir_all(&environment).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "language-julia",
                "publisher": "julialang",
                "version": extension_version,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            environment.join("Project.toml"),
            format!("[deps]\nLanguageServer = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\n"),
        )
        .unwrap();
        std::fs::write(
            environment.join("Manifest.toml"),
            format!(
                "manifest_format = \"2.0\"\n\n[[deps.LanguageServer]]\npath = \"../../../packages/LanguageServer\"\nuuid = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\nversion = \"{server_version}\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            package.join("Project.toml"),
            format!(
                "name = \"LanguageServer\"\nuuid = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\nversion = \"{server_version}\"\n"
            ),
        )
        .unwrap();
        std::fs::write(&source, "module LanguageServer\nend\n").unwrap();
        (environment.join("Project.toml"), source)
    }

    fn write_fake_julia(
        root: &Path,
        runtime_version: &str,
        server_version: &str,
        package: &Path,
    ) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        #[cfg(windows)]
        let body = format!(
            "if \"%~1\"==\"--version\" (\r\n  echo julia version {runtime_version}\r\n  exit /b 0\r\n)\r\necho {runtime_version}\r\necho {server_version}\r\necho {}",
            package.display()
        );
        #[cfg(unix)]
        let body = format!(
            "if [ \"$1\" = \"--version\" ]; then\n  echo 'julia version {runtime_version}'\n  exit 0\nfi\nprintf '%s\\n' '{runtime_version}' '{server_version}' '{}'",
            package.display()
        );
        fake_executable(root, "julia", &body)
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
        assert_eq!(
            dotnet_tool_package("csharp"),
            Some(ROSLYN_LANGUAGE_SERVER_PACKAGE)
        );
        assert_eq!(
            dotnet_tool_package(FSHARP_SERVER_ID),
            Some(FSHARP_LANGUAGE_SERVER_PACKAGE)
        );
        assert_eq!(dotnet_tool_package("rust"), None);
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

    #[test]
    fn elixir_ls_release_probe_requires_official_bounded_version() {
        let root = tempfile::tempdir().unwrap();
        let executable = write_elixir_ls_release(root.path(), "0.31.1");
        let version_file = executable.parent().unwrap().join("VERSION");

        assert_eq!(
            probe_elixir_ls_release(&executable, ">=0.31.1, <0.32.0").unwrap(),
            "0.31.1"
        );

        let wrong_launcher = executable.parent().unwrap().join("debug_adapter.bat");
        std::fs::write(&wrong_launcher, b"launcher").unwrap();
        assert!(probe_elixir_ls_release(&wrong_launcher, ">=0.31.1").is_err());

        std::fs::remove_file(&version_file).unwrap();
        assert!(probe_elixir_ls_release(&executable, ">=0.31.1").is_err());

        std::fs::write(
            &version_file,
            vec![b'1'; ELIXIR_LS_VERSION_FILE_LIMIT as usize + 1],
        )
        .unwrap();
        assert!(probe_elixir_ls_release(&executable, ">=0.31.1").is_err());

        std::fs::write(&version_file, b"not-a-version").unwrap();
        assert!(probe_elixir_ls_release(&executable, ">=0.31.1").is_err());

        std::fs::write(&version_file, b"0.30.0").unwrap();
        assert!(probe_elixir_ls_release(&executable, ">=0.31.1, <0.32.0").is_err());
    }

    #[test]
    fn vscode_elixir_ls_candidates_are_newest_first_and_official() {
        let root = tempfile::tempdir().unwrap();
        let older = write_elixir_ls_release(&root.path().join(".vscode/extensions"), "0.30.0");
        let newer =
            write_elixir_ls_release(&root.path().join(".vscode-insiders/extensions"), "0.31.1");
        let fake = root
            .path()
            .join(".vscode/extensions/not-official.elixir-ls-9.9.9/elixir-ls-release");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::write(
            fake.join(if cfg!(windows) {
                "language_server.bat"
            } else {
                "language_server.sh"
            }),
            b"launcher",
        )
        .unwrap();

        assert_eq!(
            vscode_elixir_ls_candidates_from(root.path()),
            [newer, older]
        );
    }

    #[tokio::test]
    async fn elixir_ls_reuses_the_official_vscode_release() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let executable = write_elixir_ls_release(&root.path().join(".vscode/extensions"), "0.31.1");
        let mut resolver = test_resolver(root.path());
        resolver.vscode_user_home = Some(root.path().to_path_buf());
        let server = Registry::builtin()
            .unwrap()
            .server(ELIXIR_LS_SERVER_ID)
            .unwrap()
            .clone();

        let resolution = resolver
            .resolve_vscode_elixir_ls(&server, &workspace)
            .await
            .unwrap();
        assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
        assert_eq!(resolution.path, executable);
        assert_eq!(resolution.version_output, "0.31.1");
    }

    #[test]
    fn vscode_eslint_candidates_are_newest_first_and_official() {
        let root = tempfile::tempdir().unwrap();
        let older = write_eslint_extension(&root.path().join(".vscode/extensions"), "3.0.33");
        let newer =
            write_eslint_extension(&root.path().join(".vscode-insiders/extensions"), "3.0.34");
        let fake = root
            .path()
            .join(".vscode/extensions/not-official.vscode-eslint-9.9.9/server/out/eslintServer.js");
        std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
        std::fs::write(fake, b"server").unwrap();

        assert_eq!(vscode_eslint_candidates_from(root.path()), [newer, older]);
    }

    #[test]
    fn vscode_intelephense_candidates_are_newest_first_and_official() {
        let root = tempfile::tempdir().unwrap();
        let older = write_intelephense_extension(&root.path().join(".vscode/extensions"), "1.18.4");
        let newer = write_intelephense_extension(
            &root.path().join(".vscode-insiders/extensions"),
            "1.18.5",
        );
        let fake = root.path().join(
            ".vscode/extensions/not-official.vscode-intelephense-client-9.9.9/node_modules/intelephense/lib/intelephense.js",
        );
        std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
        std::fs::write(fake, b"server").unwrap();

        assert_eq!(
            vscode_intelephense_candidates_from(root.path()),
            [newer.clone(), older.clone()]
        );
        assert!(validate_vscode_intelephense_extension(&newer, ">=1.18.5, <2.0.0").is_ok());
        assert!(validate_vscode_intelephense_extension(&older, ">=1.18.5, <2.0.0").is_err());
    }

    #[test]
    fn vscode_prisma_candidates_are_newest_first_and_official() {
        let root = tempfile::tempdir().unwrap();
        let older = write_prisma_extension(&root.path().join(".vscode/extensions"), "6.19.0");
        let newer =
            write_prisma_extension(&root.path().join(".vscode-insiders/extensions"), "31.11.0");
        let fake = root
            .path()
            .join(".vscode/extensions/not-official.prisma-99.0.0/dist/language-server/bin.js");
        std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
        std::fs::write(fake, b"server").unwrap();

        assert_eq!(
            vscode_prisma_candidates_from(root.path()),
            [newer.clone(), older.clone()]
        );
        assert!(validate_vscode_prisma_extension(&newer, ">=6.19.0, <32.0.0").is_ok());
        assert!(validate_vscode_prisma_extension(&older, ">=7.0.0, <32.0.0").is_err());
    }

    #[test]
    fn vscode_pyright_candidates_are_newest_first_and_official() {
        let root = tempfile::tempdir().unwrap();
        let older = write_pyright_extension(&root.path().join(".vscode/extensions"), "1.1.410");
        let newer =
            write_pyright_extension(&root.path().join(".vscode-insiders/extensions"), "1.1.411");
        let fake = root
            .path()
            .join(".vscode/extensions/not-official.pyright-9.9.9/dist/server.js");
        std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
        std::fs::write(fake, b"server").unwrap();

        assert_eq!(
            vscode_pyright_candidates_from(root.path()),
            [newer.clone(), older.clone()]
        );
        assert!(validate_vscode_pyright_extension(&newer, ">=1.1.300, <2.0.0").is_ok());
        assert!(validate_vscode_pyright_extension(&older, ">=1.1.411, <2.0.0").is_err());
    }

    #[test]
    fn vscode_fsharp_candidates_and_manifest_are_official_and_bounded() {
        let root = tempfile::tempdir().unwrap();
        let older =
            write_fsharp_extension(&root.path().join(".vscode/extensions"), "7.30.0", "net8.0");
        let newer_net8 = write_fsharp_extension(
            &root.path().join(".vscode-insiders/extensions"),
            "7.31.1",
            "net8.0",
        );
        let newer_net9 = write_fsharp_extension(
            &root.path().join(".vscode-insiders/extensions"),
            "7.31.1",
            "net9.0",
        );
        let fake = root
            .path()
            .join(".vscode/extensions/not-official.ionide-fsharp-9.9.9/bin/net9.0");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::write(fake.join("fsautocomplete.dll"), b"server").unwrap();

        assert_eq!(
            vscode_fsharp_candidates_from(root.path()),
            [newer_net8.clone(), newer_net9, older.clone()]
        );
        validate_vscode_fsharp_extension(&newer_net8).unwrap();
        assert!(validate_vscode_fsharp_extension(&older).is_err());

        std::fs::write(
            vscode_fsharp_extension_root(&newer_net8)
                .unwrap()
                .join("package.json"),
            br#"{"name":"Ionide-fsharp","publisher":"other","version":"7.31.1"}"#,
        )
        .unwrap();
        assert!(validate_vscode_fsharp_extension(&newer_net8).is_err());
    }

    #[test]
    fn vscode_kotlin_candidates_validate_official_bundled_servers() {
        let root = tempfile::tempdir().unwrap();
        let older = write_kotlin_extension(
            &root.path().join(".vscode/extensions"),
            "0.0.6-win32-x64",
            "0.0.6",
            "262.9593.0",
        );
        let newer = write_kotlin_extension(
            &root.path().join(".vscode-insiders/extensions"),
            "0.0.8-win32-x64",
            "0.0.8",
            "263.2689.0",
        );

        assert_eq!(
            vscode_kotlin_candidates_from(root.path()),
            [newer.clone(), older]
        );
        let layout = validate_vscode_kotlin_extension(&newer, ">=262.4739.0, <264.0.0").unwrap();
        assert!(layout.bundled_java.is_file());

        let incompatible = write_kotlin_extension(
            &root.path().join("other"),
            "0.0.8-win32-x64",
            "0.0.8",
            "264.1.0",
        );
        assert!(validate_vscode_kotlin_extension(&incompatible, ">=262.4739.0, <264.0.0").is_err());

        std::fs::write(
            layout.manifest,
            br#"{"name":"kotlin-server","publisher":"other","version":"0.0.8"}"#,
        )
        .unwrap();
        assert!(validate_vscode_kotlin_extension(&newer, ">=262.4739.0, <264.0.0").is_err());
        assert!(PRESERVED_ENV.contains(&"GRADLE_USER_HOME"));
    }

    #[test]
    fn vscode_lua_candidates_validate_official_bounded_runtime() {
        let root = tempfile::tempdir().unwrap();
        let older = write_lua_extension(
            &root.path().join(".vscode/extensions"),
            "3.18.2-win32-x64",
            "3.18.2",
        );
        let newer = write_lua_extension(
            &root.path().join(".vscode-insiders/extensions"),
            "3.19.0-win32-x64",
            "3.19.0",
        );
        assert_eq!(
            vscode_lua_candidates_from(root.path()),
            [newer.clone(), older]
        );

        let (layout, version) = validate_vscode_lua_extension(&newer, ">=3.19.0, <4.0.0").unwrap();
        assert_eq!(version, Version::new(3, 19, 0));
        validate_lua_server_version("3.19.0", &version).unwrap();
        assert!(validate_lua_server_version("3.19.1", &version).is_err());

        let mismatched = write_lua_extension(
            &root.path().join("mismatched"),
            "3.19.1-win32-x64",
            "3.19.0",
        );
        assert!(validate_vscode_lua_extension(&mismatched, ">=3.19.0, <4.0.0").is_err());

        let forged = write_lua_extension(&root.path().join("forged"), "3.19.0-win32-x64", "3.19.0");
        let forged_manifest = lua_extension_layout(&forged).unwrap().manifest;
        std::fs::write(
            forged_manifest,
            br#"{"name":"lua","publisher":"other","version":"3.19.0"}"#,
        )
        .unwrap();
        assert!(validate_vscode_lua_extension(&forged, ">=3.19.0, <4.0.0").is_err());

        let incomplete = write_lua_extension(
            &root.path().join("incomplete"),
            "3.19.0-win32-x64",
            "3.19.0",
        );
        std::fs::remove_dir_all(lua_extension_layout(&incomplete).unwrap().locale).unwrap();
        assert!(validate_vscode_lua_extension(&incomplete, ">=3.19.0, <4.0.0").is_err());

        std::fs::write(
            layout.manifest,
            vec![b' '; LUA_EXTENSION_FILE_LIMIT as usize + 1],
        )
        .unwrap();
        assert!(validate_vscode_lua_extension(&newer, ">=3.19.0, <4.0.0").is_err());

        let outside = root.path().join(if cfg!(windows) {
            "outside/lua-language-server.exe"
        } else {
            "outside/lua-language-server"
        });
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, b"launcher").unwrap();
        assert!(lua_extension_layout(&outside).is_err());
    }

    #[tokio::test]
    async fn vscode_jdtls_candidates_validate_layout_java_and_platform_suffixes() {
        let root = tempfile::tempdir().unwrap();
        let extensions = root.path().join(".vscode/extensions");
        let (older, _) = write_jdtls_extension(
            &extensions,
            "1.54.0-win32-x64",
            "1.54.0",
            "1.59.0.202606010000",
            "21.0.3",
        );
        let (newer, java) = write_jdtls_extension(
            &extensions,
            "1.55.0-win32-x64",
            "1.55.0",
            "1.60.0.202607010000",
            "21.0.3",
        );

        assert_eq!(
            vscode_jdtls_candidates_from(root.path()),
            [newer.clone(), older.clone()]
        );
        let (layout, version) =
            validate_vscode_jdtls_extension(&newer, ">=1.30.0, <2.0.0").unwrap();
        assert_eq!(version, Version::new(1, 60, 0));
        assert_eq!(
            jdtls_java_for_launcher(&newer, root.path(), Duration::from_secs(5))
                .await
                .unwrap(),
            (java, 21)
        );
        assert_eq!(java_major_version("openjdk version \"21.0.3\""), Some(21));
        assert_eq!(java_major_version("openjdk version \"20.0.2\""), Some(20));

        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut resolver = test_resolver(root.path());
        resolver.vscode_user_home = Some(root.path().to_path_buf());
        let server = Registry::builtin()
            .unwrap()
            .server(JDTLS_SERVER_ID)
            .unwrap()
            .clone();
        let resolution = resolver
            .resolve_vscode_jdtls(&server, &workspace)
            .await
            .unwrap();
        assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
        assert_eq!(resolution.path, newer);
        assert!(resolution.version_output.contains("Eclipse JDT LS 1.60.0"));

        std::fs::write(
            layout.extension_root.join("package.json"),
            br#"{"name":"java","publisher":"other","version":"1.55.0"}"#,
        )
        .unwrap();
        assert!(validate_vscode_jdtls_extension(&resolution.path, ">=1.30.0").is_err());

        std::fs::write(
            jdtls_extension_layout(&older)
                .unwrap()
                .core
                .with_file_name("org.eclipse.jdt.ls.core_1.59.1.jar"),
            b"duplicate",
        )
        .unwrap();
        assert!(jdtls_extension_layout(&older).is_err());
    }

    #[tokio::test]
    async fn julials_probe_checks_julia_and_language_server_versions() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("LanguageServer/src/LanguageServer.jl");
        std::fs::create_dir_all(package.parent().unwrap()).unwrap();
        std::fs::write(&package, "module LanguageServer\nend\n").unwrap();
        let julia = write_fake_julia(&root.path().join("bin"), "1.11.9", "5.0.0", &package);

        assert_eq!(
            julials_probe_args(None),
            [
                "--startup-file=no",
                "--history-file=no",
                "-e",
                JULIALS_PROBE_SCRIPT,
            ]
        );
        assert_eq!(
            julials_probe_timeout(Duration::from_millis(1_500)),
            JULIALS_PROBE_TIMEOUT
        );
        assert_eq!(
            julials_probe_timeout(Duration::from_secs(10)),
            Duration::from_secs(10)
        );
        let environment = root.path().join("environment");
        assert_eq!(
            julials_probe_args(Some(&environment)),
            [
                "--startup-file=no".to_owned(),
                "--history-file=no".to_owned(),
                format!("--project={}", environment.to_string_lossy()),
                "-e".to_owned(),
                JULIALS_PROBE_SCRIPT.to_owned(),
            ]
        );
        let probe = probe_julials(
            &julia,
            None,
            root.path(),
            ">=5.0.0, <6.0.0",
            ">=1.10.0",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(probe.julia_version, Version::new(1, 11, 9));
        assert_eq!(probe.server_version, Version::new(5, 0, 0));
        assert_eq!(probe.package_path, package);
        assert!(
            probe_julia_version(&julia, root.path(), Duration::from_secs(5), ">=1.12.0")
                .await
                .is_err()
        );
        assert!(
            parse_julials_probe_output(
                format!("1.9.4\n5.0.0\n{}", package.display()).as_bytes(),
                ">=5.0.0, <6.0.0",
                ">=1.10.0",
            )
            .is_err()
        );
        assert!(
            parse_julials_probe_output(
                format!("1.11.9\n4.5.0\n{}", package.display()).as_bytes(),
                ">=5.0.0, <6.0.0",
                ">=1.10.0",
            )
            .is_err()
        );
        assert!(PRESERVED_ENV.contains(&"JULIA_DEPOT_PATH"));
        assert!(PRESERVED_ENV.contains(&"JULIA_LOAD_PATH"));
        assert!(PRESERVED_ENV.contains(&"JULIA_PROJECT"));
    }

    #[tokio::test]
    async fn vscode_julials_uses_official_exact_or_fallback_environment() {
        let root = tempfile::tempdir().unwrap();
        let stable = root.path().join(".vscode/extensions");
        let insiders = root.path().join(".vscode-insiders/extensions");
        let (fallback, _) = write_julials_extension(&stable, "1.230.0", "fallback", "5.0.0");
        let (matching, source) = write_julials_extension(&insiders, "1.231.1", "v1.11", "5.1.0");
        let julia_version = Version::new(1, 11, 9);

        assert_eq!(
            vscode_julials_candidates_from(root.path(), &julia_version),
            [matching.clone(), fallback.clone()]
        );
        assert_eq!(
            vscode_julials_candidates_from(root.path(), &Version::new(1, 12, 1)),
            [fallback]
        );
        let (layout, extension_version, server_version) =
            validate_vscode_julials_environment(&matching, ">=5.0.0, <6.0.0").unwrap();
        assert_eq!(extension_version, Version::new(1, 231, 1));
        assert_eq!(server_version, Version::new(5, 1, 0));
        assert_eq!(
            julials_extension_environment(&matching).unwrap(),
            layout.environment
        );

        let julia = write_fake_julia(&root.path().join("bin"), "1.11.9", "5.1.0", &source);
        let output = probe_vscode_julials(
            &julia,
            &matching,
            root.path(),
            ">=5.0.0, <6.0.0",
            &julia_version,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(output.contains("Julia 1.11.9; LanguageServer 5.1.0"));
        assert!(output.contains("julialang.language-julia 1.231.1"));
    }

    #[test]
    fn vscode_julials_rejects_forged_oversized_and_escaping_metadata() {
        let root = tempfile::tempdir().unwrap();
        let extensions = root.path().join(".vscode/extensions");

        let (forged, _) = write_julials_extension(&extensions, "1.231.0", "v1.11", "5.1.0");
        let forged_root = julials_extension_layout(&forged).unwrap().extension_root;
        std::fs::write(
            forged_root.join("package.json"),
            br#"{"name":"language-julia","publisher":"other","version":"1.231.0"}"#,
        )
        .unwrap();
        assert!(validate_vscode_julials_environment(&forged, ">=5.0.0, <6.0.0").is_err());

        let (mismatched, _) = write_julials_extension(&extensions, "1.231.1", "v1.11", "5.1.0");
        let mismatched_root = julials_extension_layout(&mismatched)
            .unwrap()
            .extension_root;
        std::fs::write(
            mismatched_root.join("package.json"),
            br#"{"name":"language-julia","publisher":"julialang","version":"1.231.0"}"#,
        )
        .unwrap();
        assert!(validate_vscode_julials_environment(&mismatched, ">=5.0.0, <6.0.0").is_err());

        let (oversized, _) = write_julials_extension(&extensions, "1.232.0", "v1.11", "5.1.0");
        let oversized_root = julials_extension_layout(&oversized).unwrap().extension_root;
        std::fs::write(
            oversized_root.join("package.json"),
            vec![b'x'; JULIALS_FILE_LIMIT as usize + 1],
        )
        .unwrap();
        assert!(validate_vscode_julials_environment(&oversized, ">=5.0.0, <6.0.0").is_err());

        let (escaping, _) = write_julials_extension(&extensions, "1.233.0", "v1.11", "5.1.0");
        let outside = extensions.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let layout = julials_extension_layout(&escaping).unwrap();
        std::fs::write(
            &layout.manifest,
            format!(
                "[[deps.LanguageServer]]\npath = \"../../../../../outside\"\nuuid = \"{JULIALS_LANGUAGE_SERVER_UUID}\"\nversion = \"5.1.0\"\n"
            ),
        )
        .unwrap();
        assert!(validate_vscode_julials_environment(&escaping, ">=5.0.0, <6.0.0").is_err());
    }

    #[tokio::test]
    async fn eslint_probe_requires_the_official_extension_and_project_dependency() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        write_eslint_dependency(&workspace, "9.32.0");
        let executable = write_eslint_extension(root.path(), "3.0.34");

        let probe = probe_vscode_eslint_server(&executable, &workspace, ">=3.0.34, <3.1.0")
            .await
            .unwrap();
        assert_eq!(probe.version_output, "vscode-eslint 3.0.34");
        assert_eq!(probe.npm_modules_root, Some(workspace.join("node_modules")));

        let extension_root = vscode_eslint_extension_root(&executable).unwrap();
        std::fs::write(
            extension_root.join("package.json"),
            br#"{"name":"vscode-eslint","publisher":"other","version":"3.0.34"}"#,
        )
        .unwrap();
        assert!(
            probe_vscode_eslint_server(&executable, &workspace, ">=3.0.34, <3.1.0")
                .await
                .is_err()
        );

        write_eslint_extension(root.path(), "3.0.34");
        std::fs::remove_file(workspace.join("node_modules/eslint/package.json")).unwrap();
        assert!(
            probe_vscode_eslint_server(&executable, &workspace, ">=3.0.34, <3.1.0")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn eslint_reuses_the_official_vscode_server() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        write_eslint_dependency(&workspace, "9.32.0");
        let executable = write_eslint_extension(&root.path().join(".vscode/extensions"), "3.0.34");
        let mut resolver = test_resolver(root.path());
        resolver.vscode_user_home = Some(root.path().to_path_buf());
        let server = Registry::builtin()
            .unwrap()
            .server(ESLINT_SERVER_ID)
            .unwrap()
            .clone();

        let resolution = resolver
            .resolve_vscode_eslint(&server, &workspace)
            .await
            .unwrap();
        assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
        assert_eq!(resolution.path, executable);
        assert_eq!(resolution.version_output, "vscode-eslint 3.0.34");
    }

    #[test]
    fn intelephense_extension_probe_requires_official_matching_manifests() {
        let root = tempfile::tempdir().unwrap();
        let executable = write_intelephense_extension(root.path(), "1.18.5");
        let probe =
            validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").unwrap();
        assert_eq!(probe.version_output, "intelephense 1.18.5");
        assert_eq!(
            probe.modules_root,
            std::fs::canonicalize(root.path().join(format!(
                "{INTELEPHENSE_EXTENSION_PREFIX}1.18.5/node_modules"
            )))
            .unwrap()
        );

        let extension_root = vscode_intelephense_extension_root(&executable).unwrap();
        std::fs::write(
            extension_root.join("package.json"),
            br#"{"name":"vscode-intelephense-client","publisher":"other","version":"1.18.5"}"#,
        )
        .unwrap();
        assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());

        write_intelephense_extension(root.path(), "1.18.5");
        std::fs::write(
            extension_root.join("node_modules/intelephense/package.json"),
            br#"{"name":"intelephense","version":"1.18.4"}"#,
        )
        .unwrap();
        assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());

        write_intelephense_extension(root.path(), "1.18.5");
        std::fs::write(
            extension_root.join("package.json"),
            vec![b' '; 1024 * 1024 + 1],
        )
        .unwrap();
        assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());

        write_intelephense_extension(root.path(), "1.18.5");
        std::fs::remove_file(&executable).unwrap();
        assert!(validate_vscode_intelephense_extension(&executable, ">=1.18.5, <2.0.0").is_err());
    }

    #[tokio::test]
    async fn intelephense_reuses_the_official_vscode_server() {
        let root = tempfile::tempdir().unwrap();
        let executable =
            write_intelephense_extension(&root.path().join(".vscode/extensions"), "1.18.5");
        let mut resolver = test_resolver(root.path());
        resolver.config.auto_install = false;
        resolver.vscode_user_home = Some(root.path().to_path_buf());
        let server = Registry::builtin()
            .unwrap()
            .server(INTELEPHENSE_SERVER_ID)
            .unwrap()
            .clone();

        let resolution = resolver.resolve_vscode_intelephense(&server).await.unwrap();
        assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
        assert_eq!(resolution.path, executable);
        assert_eq!(resolution.version_output, "intelephense 1.18.5");
    }

    #[test]
    fn prisma_extension_probe_requires_official_manifest_and_wasm() {
        let root = tempfile::tempdir().unwrap();
        let executable = write_prisma_extension(root.path(), "31.11.0");
        assert_eq!(
            validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").unwrap(),
            "@prisma/language-server 31.11.0"
        );

        let extension_root = vscode_prisma_extension_root(&executable).unwrap();
        std::fs::write(
            extension_root.join("package.json"),
            br#"{"name":"prisma","publisher":"other","version":"31.11.0"}"#,
        )
        .unwrap();
        assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());

        write_prisma_extension(root.path(), "31.11.0");
        std::fs::write(
            extension_root.join("package.json"),
            br#"{"name":"prisma","publisher":"Prisma","version":"31.10.0"}"#,
        )
        .unwrap();
        assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());

        write_prisma_extension(root.path(), "31.11.0");
        std::fs::remove_file(
            extension_root.join("dist/language-server/prisma_schema_build_bg.wasm"),
        )
        .unwrap();
        assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());

        write_prisma_extension(root.path(), "31.11.0");
        std::fs::remove_file(&executable).unwrap();
        assert!(validate_vscode_prisma_extension(&executable, ">=6.19.0, <32.0.0").is_err());
    }

    #[tokio::test]
    async fn prisma_reuses_the_official_vscode_server() {
        let root = tempfile::tempdir().unwrap();
        let executable = write_prisma_extension(&root.path().join(".vscode/extensions"), "31.11.0");
        let mut resolver = test_resolver(root.path());
        resolver.config.auto_install = false;
        resolver.vscode_user_home = Some(root.path().to_path_buf());
        let server = Registry::builtin()
            .unwrap()
            .server(PRISMA_SERVER_ID)
            .unwrap()
            .clone();

        let resolution = resolver.resolve_vscode_prisma(&server).await.unwrap();
        assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
        assert_eq!(resolution.path, executable);
        assert_eq!(resolution.version_output, "@prisma/language-server 31.11.0");
        assert!(resolution.npm_modules_root.is_none());
    }

    #[test]
    fn pyright_extension_probe_requires_official_matching_manifest() {
        let root = tempfile::tempdir().unwrap();
        let executable = write_pyright_extension(root.path(), "1.1.411");
        assert_eq!(
            validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").unwrap(),
            "pyright 1.1.411"
        );

        let extension_root = vscode_pyright_extension_root(&executable).unwrap();
        std::fs::write(
            extension_root.join("package.json"),
            br#"{"name":"pyright","publisher":"other","version":"1.1.411"}"#,
        )
        .unwrap();
        assert!(validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").is_err());

        write_pyright_extension(root.path(), "1.1.411");
        std::fs::write(
            extension_root.join("package.json"),
            br#"{"name":"pyright","publisher":"ms-pyright","version":"1.1.410"}"#,
        )
        .unwrap();
        assert!(validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").is_err());

        write_pyright_extension(root.path(), "1.1.411");
        std::fs::remove_file(&executable).unwrap();
        assert!(validate_vscode_pyright_extension(&executable, ">=1.1.300, <2.0.0").is_err());
    }

    #[tokio::test]
    async fn pyright_reuses_the_official_vscode_server() {
        let root = tempfile::tempdir().unwrap();
        let executable =
            write_pyright_extension(&root.path().join(".vscode/extensions"), "1.1.411");
        let mut resolver = test_resolver(root.path());
        resolver.config.auto_install = false;
        resolver.vscode_user_home = Some(root.path().to_path_buf());
        let server = Registry::builtin()
            .unwrap()
            .server(PYRIGHT_SERVER_ID)
            .unwrap()
            .clone();

        let resolution = resolver.resolve_vscode_pyright(&server).await.unwrap();
        assert_eq!(resolution.source, ExecutableSource::VsCodeExtension);
        assert_eq!(resolution.path, executable);
        assert_eq!(resolution.version_output, "pyright 1.1.411");
        assert!(resolution.npm_modules_root.is_none());
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
            ("golang.org/x/tools/gopls v0.23.0", Version::new(0, 23, 0)),
            ("clangd version 18.1.8", Version::new(18, 1, 8)),
            (
                "rust-analyzer 1.88.0 (6b00bc388 2025-06-23)",
                Version::new(1, 88, 0),
            ),
            ("clojure-lsp 2026.07.06-14.34.19", Version::new(2026, 7, 6)),
            (
                "Dart SDK version: 3.8.1 (stable) on windows_x64",
                Version::new(3, 8, 1),
            ),
            (
                "deno 2.8.1 (stable, release, x86_64-pc-windows-msvc)\nv8 14.2.231.17-rusty\ntypescript 5.9.2",
                Version::new(2, 8, 1),
            ),
            ("gleam 1.18.1", Version::new(1, 18, 1)),
            ("LS-262.9593.0", Version::new(262, 9593, 0)),
            ("ILS-263.2689.0", Version::new(263, 2689, 0)),
            ("2.14.0.0", Version::new(2, 14, 0)),
            ("1.27.0", Version::new(1, 27, 0)),
        ] {
            assert_eq!(parse_version(output), Some(expected));
        }
        assert!(validate_version_output("tool v1.4.0", ">=1.0.0, <2.0.0").is_ok());
        assert!(validate_version_output("tool v2.0.0", ">=1.0.0, <2.0.0").is_err());
        assert!(validate_version_output("v20.0.0", ">=20.0.0").is_ok());
        assert!(validate_version_output("v19.9.0", ">=20.0.0").is_err());
        assert!(
            validate_version_output("clojure-lsp 2026.07.06-14.34.19", ">=2026.7.6, <2027.0.0")
                .is_ok()
        );
        assert!(validate_version_output("2.14.0.0", ">=2.0.0, <3.0.0").is_ok());
        assert!(validate_version_output("3.0.0.0", ">=2.0.0, <3.0.0").is_err());
        assert_eq!(parse_version("2.14.0.0.1"), None);
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
    fn oxlint_version_output_is_supported() {
        assert_eq!(
            parse_version("Version: 1.78.0"),
            Some(Version::new(1, 78, 0))
        );
        assert!(validate_version_output("Version: 1.78.0", ">=1.78.0, <2.0.0").is_ok());
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

    #[cfg(windows)]
    #[test]
    fn windows_executable_candidates_include_batch_launchers() {
        assert_eq!(
            executable_names("language_server"),
            [
                "language_server.exe",
                "language_server.cmd",
                "language_server.bat",
                "language_server",
            ]
        );
    }

    #[test]
    fn elixir_ls_version_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let executable = write_elixir_ls_release(directory.path(), "0.31.1");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(ELIXIR_LS_SERVER_ID).unwrap();
        let first = resolution_fingerprint(server, directory.path(), Some(&executable));
        std::fs::write(executable.parent().unwrap().join("VERSION"), b"0.31.2").unwrap();
        let second = resolution_fingerprint(server, directory.path(), Some(&executable));
        assert_ne!(first, second);
    }

    #[test]
    fn eslint_manifest_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        write_eslint_dependency(&workspace, "9.32.0");
        let executable = write_eslint_extension(directory.path(), "3.0.34");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(ESLINT_SERVER_ID).unwrap();
        let first = resolution_fingerprint(server, &workspace, Some(&executable));
        write_eslint_dependency(&workspace, "9.33.0");
        let second = resolution_fingerprint(server, &workspace, Some(&executable));
        assert_ne!(first, second);
    }

    #[test]
    fn intelephense_manifest_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let executable = write_intelephense_extension(directory.path(), "1.18.5");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(INTELEPHENSE_SERVER_ID).unwrap();
        let manifest = vscode_intelephense_extension_root(&executable)
            .unwrap()
            .join("node_modules/intelephense/package.json");
        let first = resolution_fingerprint(server, &workspace, Some(&executable));
        std::fs::write(manifest, br#"{"name":"intelephense","version":"1.18.6"}"#).unwrap();
        let second = resolution_fingerprint(server, &workspace, Some(&executable));
        assert_ne!(first, second);
    }

    #[test]
    fn prisma_wasm_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let executable = write_prisma_extension(directory.path(), "31.11.0");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(PRISMA_SERVER_ID).unwrap();
        let wasm = vscode_prisma_extension_root(&executable)
            .unwrap()
            .join("dist/language-server/prisma_schema_build_bg.wasm");
        let first = resolution_fingerprint(server, &workspace, Some(&executable));
        std::fs::write(wasm, b"different-wasm").unwrap();
        let second = resolution_fingerprint(server, &workspace, Some(&executable));
        assert_ne!(first, second);
    }

    #[test]
    fn pyright_manifest_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let executable = write_pyright_extension(directory.path(), "1.1.411");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(PYRIGHT_SERVER_ID).unwrap();
        let manifest = vscode_pyright_extension_root(&executable)
            .unwrap()
            .join("package.json");
        let first = resolution_fingerprint(server, &workspace, Some(&executable));
        std::fs::write(
            manifest,
            br#"{"name":"pyright","publisher":"ms-pyright","version":"1.1.410"}"#,
        )
        .unwrap();
        let second = resolution_fingerprint(server, &workspace, Some(&executable));
        assert_ne!(first, second);
    }

    #[test]
    fn jdtls_core_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let (launcher, _) = write_jdtls_extension(
            directory.path(),
            "1.55.0-win32-x64",
            "1.55.0",
            "1.60.0.202607010000",
            "21.0.3",
        );
        let registry = Registry::builtin().unwrap();
        let server = registry.server(JDTLS_SERVER_ID).unwrap();
        let core = jdtls_extension_layout(&launcher).unwrap().core;
        let first = resolution_fingerprint(server, &workspace, Some(&launcher));
        std::fs::write(core, b"different-core").unwrap();
        let second = resolution_fingerprint(server, &workspace, Some(&launcher));
        assert_ne!(first, second);
    }

    #[test]
    fn julials_environment_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let (project, _) = write_julials_extension(directory.path(), "1.231.1", "v1.11", "5.1.0");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(JULIALS_SERVER_ID).unwrap();
        let manifest = julials_extension_layout(&project).unwrap().manifest;
        let first = resolution_fingerprint(server, &workspace, Some(&project));
        let mut contents = std::fs::read_to_string(&manifest).unwrap();
        contents.push_str("\n# changed\n");
        std::fs::write(manifest, contents).unwrap();
        let second = resolution_fingerprint(server, &workspace, Some(&project));
        assert_ne!(first, second);
    }

    #[test]
    fn kotlin_bundle_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let launcher =
            write_kotlin_extension(directory.path(), "0.0.8-win32-x64", "0.0.8", "263.2689.0");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(KOTLIN_LS_SERVER_ID).unwrap();
        let build_file = kotlin_extension_layout(&launcher).unwrap().build_file;
        let first = resolution_fingerprint(server, &workspace, Some(&launcher));
        std::fs::write(build_file, b"ILS-263.2689.1\n").unwrap();
        let second = resolution_fingerprint(server, &workspace, Some(&launcher));
        assert_ne!(first, second);
    }

    #[test]
    fn lua_runtime_identity_changes_the_resolution_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let executable = write_lua_extension(directory.path(), "3.19.0-win32-x64", "3.19.0");
        let registry = Registry::builtin().unwrap();
        let server = registry.server(LUA_LS_SERVER_ID).unwrap();
        let server_main = lua_extension_layout(&executable).unwrap().server_main;
        let first = resolution_fingerprint(server, &workspace, Some(&executable));
        std::fs::write(server_main, b"return false -- changed").unwrap();
        let second = resolution_fingerprint(server, &workspace, Some(&executable));
        assert_ne!(first, second);
    }
}

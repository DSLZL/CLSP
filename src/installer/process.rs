//! Bounded child-process execution shared by installer probes and LSP setup.

use std::{
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

use crate::protocol::ClspError;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub(crate) const OUTPUT_LIMIT: usize = 4_096;
pub(crate) const PRESERVED_ENV: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
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
    "BUNDLE_GEMFILE",
    "BUNDLE_PATH",
    "BUNDLE_WITH",
    "BUNDLE_WITHOUT",
    "GEM_HOME",
    "GEM_PATH",
    "RUBYGEMS_GEMDEPS",
    "RUBYLIB",
    "RUBYOPT",
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

#[derive(Debug)]
pub(super) struct CommandOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
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

pub(super) async fn run_command(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    duration: Duration,
) -> Result<CommandOutput, ClspError> {
    let mut command = Command::new(executable);
    command.args(args).current_dir(cwd);
    sanitize_command(&mut command);
    let mut child = command.spawn().map_err(|error| {
        super::server_error(format!("cannot start {}: {error}", executable.display()))
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| super::server_error("child stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| super::server_error("child stderr was not captured"))?;
    let stdout = tokio::spawn(read_prefix(stdout));
    let stderr = tokio::spawn(read_prefix(stderr));

    let result = timeout(duration, child.wait()).await;
    let timed_out = result.is_err();
    let status = match result {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(super::server_error(format!(
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
        return Err(super::server_error(format!(
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

pub(super) async fn run_checked(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    duration: Duration,
    label: &str,
) -> Result<CommandOutput, ClspError> {
    let output = run_command(executable, args, cwd, duration).await?;
    if !output.status.success() {
        return Err(super::server_error(format!(
            "{label} exited with {}; {}",
            output.status,
            command_output_detail(&output)
        )));
    }
    Ok(output)
}

pub(super) fn command_output_detail(output: &CommandOutput) -> String {
    format!(
        "stdout: {}; stderr: {}",
        bounded_text(&output.stdout),
        bounded_text(&output.stderr)
    )
}

pub(super) fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(OUTPUT_LIMIT)]).replace(['\r', '\n'], " ")
}

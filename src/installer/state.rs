//! Workspace-scoped state paths and atomic publication.

use std::path::{Path, PathBuf};

use crate::protocol::{ClspError, ErrorCode};

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
            std::fs::create_dir_all(path).map_err(super::server_error)?;
        }
        Ok(paths)
    }
}

pub(super) async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ClspError> {
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        super::TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    tokio::fs::write(&temp, bytes)
        .await
        .map_err(super::server_error)?;
    crate::ipc::atomic_replace(&temp, path).map_err(super::server_error)
}

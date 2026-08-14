//! Bounded archive and managed-artifact operations.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::protocol::ClspError;

pub(super) const ARCHIVE_DOWNLOAD_LIMIT: u64 = 32 * 1024 * 1024;
pub(super) const ARCHIVE_EXTRACT_LIMIT: u64 = 512 * 1024 * 1024;
const ARCHIVE_ENTRY_LIMIT: usize = 4_096;

pub(super) fn system_curl() -> Result<PathBuf, ClspError> {
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let curl = PathBuf::from(system_root).join("System32/curl.exe");
        if curl.is_file() {
            return Ok(curl);
        }
    }
    let program = if cfg!(windows) { "curl.exe" } else { "curl" };
    which::which(program).map_err(|_| {
        super::runtime_error(
            "curl is required for CLSP managed archive installs; install the language server locally or configure its executable",
        )
    })
}

pub(super) async fn verify_file_sha256(path: &Path, expected: &str) -> Result<(), ClspError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(super::server_error)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await.map_err(super::server_error)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = hex::encode(digest.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(super::server_error(format!(
            "archive SHA-256 mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

pub(super) fn extract_zip(
    archive_path: &Path,
    destination: &Path,
    expanded_limit: u64,
) -> Result<(), ClspError> {
    let file = std::fs::File::open(archive_path).map_err(super::server_error)?;
    let mut archive = zip::ZipArchive::new(file).map_err(super::server_error)?;
    if archive.len() > ARCHIVE_ENTRY_LIMIT {
        return Err(super::server_error("archive contains too many entries"));
    }
    std::fs::create_dir_all(destination).map_err(super::server_error)?;
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(super::server_error)?;
        if entry.is_symlink() {
            return Err(super::server_error("archive contains a symbolic link"));
        }
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| super::server_error("archive contains an unsafe path"))?;
        if !entry.is_dir() {
            expanded = expanded
                .checked_add(entry.size())
                .ok_or_else(|| super::server_error("archive expanded size overflow"))?;
            if expanded > expanded_limit {
                return Err(super::server_error(
                    "archive exceeds the expanded size limit",
                ));
            }
        }
        let output = destination.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&output).map_err(super::server_error)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(super::server_error)?;
        }
        let mut output_file = std::fs::File::create(&output).map_err(super::server_error)?;
        let copied = std::io::copy(&mut entry, &mut output_file).map_err(super::server_error)?;
        if copied != entry.size() {
            return Err(super::server_error(
                "archive entry size changed during extraction",
            ));
        }
    }
    Ok(())
}

pub(super) fn github_zip_candidate(
    artifacts: &Path,
    server_id: &str,
    version: &str,
    executable: &str,
) -> PathBuf {
    artifacts.join(server_id).join(version).join(executable)
}

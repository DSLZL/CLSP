//! Shared test-only fixture primitives.
//!
//! Keep filesystem lifetime and naming policy in one place so individual
//! contract tests describe behavior instead of repeating `tempfile` setup.

// Each Cargo integration target compiles this module independently; not every
// target needs every filesystem primitive.
#![allow(dead_code)]

use std::{fs, io, path::Path};

pub use tempfile::TempDir;

pub fn tempdir() -> std::io::Result<TempDir> {
    tempfile::Builder::new().prefix("clsp-test-").tempdir()
}

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    fs::write(path, contents)
}

pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    fs::create_dir(path)
}

pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    fs::create_dir_all(path)
}

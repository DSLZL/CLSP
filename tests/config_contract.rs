use std::fs;
use std::path::Path;

use clsp::config::{Config, ConfigOverrides, IdeConfig};
use clsp::protocol::ErrorCode;

#[test]
fn project_config_overrides_defaults() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".clsp.toml"),
        "prewarm = false\n[install]\ncommand_timeout_seconds = 60\n",
    )
    .unwrap();

    let config = Config::load(dir.path(), ConfigOverrides::default()).unwrap();
    assert!(config.enabled);
    assert!(config.auto_install);
    assert!(!config.prewarm);
    assert_eq!(config.install.command_timeout_seconds, 60);
    assert_eq!(config.discovery.max_initial_ms, 300);
}

#[test]
fn rejects_removed_managed_install_settings() {
    for source in [
        "[runtime]\npolicy = 'managed-only'\n",
        "[install]\ndownload_timeout_seconds = 120\n",
        "[lsp.rust]\npolicy = 'local-only'\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".clsp.toml"), source).unwrap();
        assert_eq!(
            Config::load(dir.path(), ConfigOverrides::default())
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfig
        );
    }
}

#[test]
fn rejects_unknown_or_phase_three_settings() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".clsp.toml"), "offline = true\n").unwrap();
    let error = Config::load(dir.path(), ConfigOverrides::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidConfig);

    let mut config = Config::default();
    config.discovery.max_entries = usize::MAX;
    assert_eq!(
        config.validate().unwrap_err().code,
        ErrorCode::InvalidConfig
    );
}

#[test]
fn ide_denied_paths_have_secure_replaceable_defaults() {
    let defaults = IdeConfig::default();
    assert!(defaults.is_denied(Path::new(".git/config")).unwrap());
    assert!(defaults.is_denied(Path::new("nested/.env.local")).unwrap());
    assert!(defaults.is_denied(Path::new("certs/client.pem")).unwrap());
    assert!(!defaults.is_denied(Path::new("src/env.rs")).unwrap());

    let custom = IdeConfig {
        denied_paths: vec!["private/**".into()],
    };
    assert!(!custom.is_denied(Path::new(".env")).unwrap());
    assert!(custom.is_denied(Path::new("private/note.txt")).unwrap());
}

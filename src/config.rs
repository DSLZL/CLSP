use std::{collections::BTreeMap, fs, path::Path};

use ignore::gitignore::GitignoreBuilder;
use serde::{Deserialize, Serialize};

use crate::protocol::{ClspError, ErrorCode};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub auto_install: bool,
    pub prewarm: bool,
    pub runtime: RuntimeConfig,
    pub install: InstallConfig,
    pub discovery: DiscoveryConfig,
    pub diagnostics: DiagnosticsConfig,
    pub lifecycle: LifecycleConfig,
    pub limits: LimitsConfig,
    pub tui: TuiConfig,
    pub ide: IdeConfig,
    pub lsp: BTreeMap<String, ServerOverride>,
}

impl Config {
    pub fn load(workspace: &Path, overrides: ConfigOverrides) -> Result<Self, ClspError> {
        let mut value = toml::Value::try_from(Self::default()).map_err(config_error)?;

        if let Some(path) = user_config_path() {
            merge_file(&mut value, &path)?;
        }
        merge_file(&mut value, &workspace.join(".clsp.toml"))?;

        let mut config: Self = value.try_into().map_err(config_error)?;
        if let Some(enabled) = overrides.enabled {
            config.enabled = enabled;
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ClspError> {
        if !(1..=30_000).contains(&self.discovery.max_initial_ms)
            || !(1..=1_000_000).contains(&self.discovery.max_entries)
            || !(1..=64).contains(&self.discovery.max_depth)
        {
            return Err(ClspError::new(
                ErrorCode::InvalidConfig,
                "discovery limits must be greater than zero",
            ));
        }
        if !(1..=3_600).contains(&self.install.command_timeout_seconds)
            || !(1..=30_000).contains(&self.runtime.probe_timeout_ms)
            || !(1..=100).contains(&self.diagnostics.max_files)
            || !(1..=1_000).contains(&self.diagnostics.max_per_file)
            || self.diagnostics.include_related_files > 100
            || self.diagnostics.wait_ms > 60_000
        {
            return Err(ClspError::new(
                ErrorCode::InvalidConfig,
                "install and diagnostic limits are invalid",
            ));
        }
        if self.limits.max_response_bytes < 64 * 1024
            || self.limits.max_response_bytes > 64 * 1024 * 1024
            || !(1..=64 * 1024 * 1024).contains(&self.limits.max_file_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.limits.max_hook_input_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.limits.max_stderr_bytes)
            || !(1..=60).contains(&self.tui.refresh_hz_active)
            || !(1..=1_000).contains(&self.tui.recent_events)
        {
            return Err(ClspError::new(
                ErrorCode::InvalidConfig,
                "response, file, hook, stderr, and TUI limits must be bounded",
            ));
        }
        if !(5..=86_400).contains(&self.lifecycle.session_lease_seconds)
            || !(1..=604_800).contains(&self.lifecycle.server_idle_seconds)
            || !(1..=604_800).contains(&self.lifecycle.broker_idle_seconds)
        {
            return Err(ClspError::new(
                ErrorCode::InvalidConfig,
                "lifecycle timeouts are outside supported bounds",
            ));
        }
        self.ide.validate()?;
        Ok(())
    }

    pub fn ensure_enabled(&self) -> Result<(), ClspError> {
        if self.enabled {
            Ok(())
        } else {
            Err(ClspError::new(
                ErrorCode::InvalidConfig,
                "CLSP is disabled by configuration",
            ))
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_install: true,
            prewarm: true,
            runtime: RuntimeConfig::default(),
            install: InstallConfig::default(),
            discovery: DiscoveryConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            lifecycle: LifecycleConfig::default(),
            limits: LimitsConfig::default(),
            tui: TuiConfig::default(),
            ide: IdeConfig::default(),
            lsp: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConfigOverrides {
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdeConfig {
    pub denied_paths: Vec<String>,
}

impl IdeConfig {
    pub fn is_denied(&self, relative_path: &Path) -> Result<bool, ClspError> {
        let mut builder = GitignoreBuilder::new("");
        for pattern in &self.denied_paths {
            builder
                .add_line(None, pattern)
                .map_err(|error| config_error(format!("invalid IDE denied path: {error}")))?;
        }
        let matcher = builder
            .build()
            .map_err(|error| config_error(format!("invalid IDE denied paths: {error}")))?;
        Ok(matcher
            .matched_path_or_any_parents(relative_path, false)
            .is_ignore())
    }

    fn validate(&self) -> Result<(), ClspError> {
        if self.denied_paths.len() > 128
            || self
                .denied_paths
                .iter()
                .any(|pattern| pattern.is_empty() || pattern.len() > 512 || pattern.contains('\0'))
        {
            return Err(config_error(
                "IDE denied paths are outside supported bounds",
            ));
        }
        self.is_denied(Path::new("validation-only"))?;
        Ok(())
    }
}

impl Default for IdeConfig {
    fn default() -> Self {
        Self {
            denied_paths: [
                ".git/**",
                "**/.env",
                "**/.env.*",
                "**/*.pem",
                "**/*.key",
                "**/*.p12",
                "**/*.pfx",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub probe_timeout_ms: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            probe_timeout_ms: 1_500,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstallConfig {
    pub command_timeout_seconds: u64,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            command_timeout_seconds: 180,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryConfig {
    pub max_initial_ms: u64,
    pub max_entries: usize,
    pub max_depth: usize,
    pub respect_gitignore: bool,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            max_initial_ms: 300,
            max_entries: 100_000,
            max_depth: 8,
            respect_gitignore: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiagnosticsConfig {
    pub minimum_severity: crate::protocol::DiagnosticSeverity,
    pub wait_ms: u64,
    pub max_files: usize,
    pub max_per_file: usize,
    pub include_related_files: usize,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            minimum_severity: crate::protocol::DiagnosticSeverity::Error,
            wait_ms: 5_000,
            max_files: 5,
            max_per_file: 20,
            include_related_files: 2,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleConfig {
    pub session_lease_seconds: u64,
    pub server_idle_seconds: u64,
    pub broker_idle_seconds: u64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            session_lease_seconds: 120,
            server_idle_seconds: 1_200,
            broker_idle_seconds: 900,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_response_bytes: usize,
    pub max_file_bytes: u64,
    pub max_hook_input_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_response_bytes: 4 * 1024 * 1024,
            max_file_bytes: 4 * 1024 * 1024,
            max_hook_input_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TuiConfig {
    pub refresh_hz_active: u16,
    pub recent_events: usize,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            refresh_hz_active: 10,
            recent_events: 100,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerOverride {
    pub enabled: Option<bool>,
    pub executable: Option<std::path::PathBuf>,
}

fn merge_file(base: &mut toml::Value, path: &Path) -> Result<(), ClspError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(config_error(error)),
    };
    let overlay: toml::Value = toml::from_str(&source).map_err(config_error)?;
    merge_value(base, overlay);
    Ok(())
}

fn merge_value(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn user_config_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|path| {
        let mut path = std::path::PathBuf::from(path);
        path.push("clsp");
        path.push("config.toml");
        path
    })
}

fn config_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::InvalidConfig, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

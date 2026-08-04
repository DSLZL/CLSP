use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    installer::sanitize_command,
    ipc::{apply_user_system_dacl, atomic_replace, verify_user_system_dacl},
    protocol::{ClspError, ErrorCode},
    workspace::Workspace,
};

const MARKER: &str = "# clsp-ide-bridge: managed-v1";
const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;
const MAX_VSIX_BYTES: u64 = 128 * 1024 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub async fn run(workspace_path: &Path) -> anyhow::Result<()> {
    let workspace = Workspace::open(workspace_path)?;
    let current_executable = fs::canonicalize(std::env::current_exe()?)?;
    let path_executable = which::which("clsp")
        .map_err(|_| setup_error("clsp must be available on PATH before setup"))?;
    let path_executable = fs::canonicalize(path_executable)?;
    anyhow::ensure!(
        current_executable == path_executable,
        "clsp on PATH does not resolve to the running executable"
    );
    let code_executable = resolve_code_executable()?;
    let code_cli_script = child_process_path(&resolve_code_cli_script(&code_executable)?);
    let vsix = current_executable
        .parent()
        .ok_or_else(|| setup_error("cannot locate the CLSP installation directory"))?
        .join("clsp-ide.vsix");
    validate_vsix(&vsix, env!("CARGO_PKG_VERSION"))?;
    let vsix_argument = child_process_path(&vsix);

    let codex_dir = workspace.root().join(".codex");
    let config_path = codex_dir.join("config.toml");
    let hooks_path = codex_dir.join("hooks.json");
    let config_source = read_optional_utf8(&config_path, MAX_CONFIG_BYTES)?;
    let hooks_source = read_optional_utf8(&hooks_path, MAX_CONFIG_BYTES)?;
    let merged_config = merge_codex_config(&config_source)?;
    let merged_hooks = merge_hooks(&hooks_source)?;

    let mut install = tokio::process::Command::new(&code_executable);
    sanitize_command(&mut install);
    install
        .env("ELECTRON_RUN_AS_NODE", "1")
        .arg(&code_cli_script)
        .arg("--install-extension")
        .arg(&vsix_argument)
        .arg("--force")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let output = tokio::time::timeout(std::time::Duration::from_secs(120), install.output())
        .await
        .map_err(|_| setup_error("VS Code extension installation timed out"))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(4_096)]);
        anyhow::bail!("VS Code extension installation failed: {}", stderr.trim());
    }

    fs::create_dir_all(&codex_dir)?;
    let mut changed = Vec::new();
    if merged_config != config_source {
        atomic_write(&config_path, merged_config.as_bytes(), false)?;
        changed.push(config_path.clone());
    }
    if merged_hooks != hooks_source {
        atomic_write(&hooks_path, merged_hooks.as_bytes(), false)?;
        changed.push(hooks_path.clone());
    }
    write_install_locator(&current_executable)?;

    if changed.is_empty() {
        println!("CLSP project configuration is already current.");
    } else {
        println!("CLSP updated:");
        for path in changed {
            println!("  {}", path.display());
        }
    }
    println!("Review and trust the project hooks with Codex /hooks, then reload VS Code.");
    Ok(())
}

fn resolve_code_executable() -> Result<PathBuf, ClspError> {
    let launcher = which::which("code")
        .map_err(|_| setup_error("VS Code CLI 'code' is not available on PATH"))?;
    let launcher = fs::canonicalize(launcher).map_err(setup_error)?;
    if launcher
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("exe"))
    {
        return Ok(launcher);
    }
    let product = launcher
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| value.to_ascii_lowercase().contains("insiders"))
        .map(|_| "Code - Insiders.exe")
        .unwrap_or("Code.exe");
    let parent = launcher
        .parent()
        .ok_or_else(|| setup_error("VS Code CLI path has no parent"))?;
    for candidate in [parent.join(product), parent.join("..").join(product)] {
        if candidate.is_file() {
            return fs::canonicalize(candidate).map_err(setup_error);
        }
    }
    Err(setup_error(
        "could not resolve Code.exe behind the VS Code CLI launcher",
    ))
}

fn resolve_code_cli_script(executable: &Path) -> Result<PathBuf, ClspError> {
    let root = executable
        .parent()
        .ok_or_else(|| setup_error("Code.exe path has no parent"))?;
    let mut candidates = Vec::new();
    let direct = root
        .join("resources")
        .join("app")
        .join("out")
        .join("cli.js");
    if direct.is_file() {
        candidates.push(direct);
    }
    for entry in fs::read_dir(root).map_err(setup_error)? {
        let entry = entry.map_err(setup_error)?;
        if !entry.file_type().map_err(setup_error)?.is_dir() {
            continue;
        }
        let candidate = entry
            .path()
            .join("resources")
            .join("app")
            .join("out")
            .join("cli.js");
        if candidate.is_file() {
            candidates.push(candidate);
        }
    }
    candidates.sort();
    candidates.dedup();
    if candidates.len() != 1 {
        return Err(setup_error(
            "could not resolve a unique VS Code CLI script behind Code.exe",
        ));
    }
    fs::canonicalize(candidates.pop().unwrap()).map_err(setup_error)
}

fn child_process_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::{
            ffi::OsString,
            os::windows::ffi::{OsStrExt, OsStringExt},
        };

        let units: Vec<_> = path.as_os_str().encode_wide().collect();
        let verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        if !units.starts_with(&verbatim) {
            return path.to_path_buf();
        }
        let unc = [b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];
        let normalized = if units[verbatim.len()..].starts_with(&unc) {
            let mut value = vec![b'\\' as u16, b'\\' as u16];
            value.extend_from_slice(&units[verbatim.len() + unc.len()..]);
            value
        } else {
            units[verbatim.len()..].to_vec()
        };
        PathBuf::from(OsString::from_wide(&normalized))
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}

fn validate_vsix(path: &Path, expected_version: &str) -> Result<(), ClspError> {
    let metadata = fs::metadata(path).map_err(|_| {
        setup_error(format!(
            "bundled VSIX is missing beside clsp.exe: {}",
            path.display()
        ))
    })?;
    if !metadata.is_file() || metadata.len() > MAX_VSIX_BYTES {
        return Err(setup_error("bundled VSIX is not a bounded regular file"));
    }
    let file = fs::File::open(path).map_err(setup_error)?;
    let mut archive = zip::ZipArchive::new(file).map_err(setup_error)?;
    let mut manifest = archive
        .by_name("extension/package.json")
        .map_err(|_| setup_error("VSIX does not contain extension/package.json"))?;
    if manifest.size() > 1024 * 1024 {
        return Err(setup_error("VSIX extension manifest exceeds its limit"));
    }
    let mut bytes = Vec::with_capacity(manifest.size() as usize);
    manifest.read_to_end(&mut bytes).map_err(setup_error)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(setup_error)?;
    if value.get("publisher").and_then(Value::as_str) != Some("clsp")
        || value.get("name").and_then(Value::as_str) != Some("clsp-ide")
        || value.get("version").and_then(Value::as_str) != Some(expected_version)
    {
        return Err(setup_error(
            "bundled VSIX identity or version does not match clsp.exe",
        ));
    }
    Ok(())
}

fn merge_codex_config(source: &str) -> Result<String, ClspError> {
    let parsed: toml::Value = toml::from_str(source).map_err(setup_error)?;
    if parsed.get("hooks").is_some() {
        return Err(setup_error(
            "inline TOML hooks already exist; migrate them to .codex/hooks.json before running setup",
        ));
    }
    let clsp_entry = mcp_entry(&parsed, "clsp");
    if clsp_entry.is_some_and(|entry| !entry_is_clsp(entry)) {
        return Err(setup_error(
            "mcp_servers.clsp is owned by a different command",
        ));
    }
    let legacy_entry = mcp_entry(&parsed, "lsp").filter(|entry| entry_is_clsp(entry));
    if clsp_entry.is_some() && legacy_entry.is_some() {
        return Err(setup_error(
            "both mcp_servers.clsp and a legacy CLSP mcp_servers.lsp entry exist",
        ));
    }

    let marker_count = source.lines().filter(|line| line.trim() == MARKER).count();
    if marker_count > 1 {
        return Err(setup_error("multiple CLSP IDE setup markers exist"));
    }
    let target = if marker_count == 1 {
        marker_target(source)?
    } else if clsp_entry.is_some() {
        "clsp"
    } else if legacy_entry.is_some() {
        "lsp"
    } else {
        "clsp"
    };
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let block = managed_mcp_block(target, newline);
    let range = if marker_count == 1 {
        Some(table_range(source, target, true)?)
    } else if (target == "clsp" && clsp_entry.is_some())
        || (target == "lsp" && legacy_entry.is_some())
    {
        Some(table_range(source, target, false)?)
    } else {
        None
    };
    if let Some(range) = range {
        return Ok(format!(
            "{}{}{}",
            &source[..range.start],
            block,
            &source[range.end..]
        ));
    }
    let mut output = source.to_owned();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push_str(newline);
    }
    if !output.is_empty() && !output.ends_with(&format!("{newline}{newline}")) {
        output.push_str(newline);
    }
    output.push_str(&block);
    Ok(output)
}

fn mcp_entry<'a>(parsed: &'a toml::Value, name: &str) -> Option<&'a toml::Value> {
    parsed
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .and_then(|servers| servers.get(name))
}

fn entry_is_clsp(entry: &toml::Value) -> bool {
    entry
        .get("command")
        .and_then(toml::Value::as_str)
        .is_some_and(executable_is_clsp)
}

fn executable_is_clsp(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("clsp") || value.eq_ignore_ascii_case("clsp.exe")
        })
}

fn marker_target(source: &str) -> Result<&'static str, ClspError> {
    let lines = line_ranges(source);
    let marker = lines
        .iter()
        .position(|line| line.text == MARKER)
        .ok_or_else(|| setup_error("CLSP IDE setup marker is missing"))?;
    let header = lines
        .iter()
        .skip(marker + 1)
        .find(|line| !line.text.is_empty())
        .map(|line| line.text)
        .ok_or_else(|| setup_error("CLSP IDE setup marker has no MCP table"))?;
    match header {
        "[mcp_servers.clsp]" => Ok("clsp"),
        "[mcp_servers.lsp]" => Ok("lsp"),
        _ => Err(setup_error(
            "CLSP IDE setup marker is not followed by an owned MCP table",
        )),
    }
}

fn managed_mcp_block(target: &str, newline: &str) -> String {
    [
        MARKER.to_owned(),
        format!("[mcp_servers.{target}]"),
        "command = \"clsp\"".into(),
        "args = [\"mcp\", \"--workspace\", \".\"]".into(),
        "cwd = \".\"".into(),
        "enabled = true".into(),
        "required = false".into(),
        "startup_timeout_sec = 10".into(),
        "tool_timeout_sec = 120".into(),
        "default_tools_approval_mode = \"auto\"".into(),
    ]
    .join(newline)
        + newline
}

struct SourceLine<'a> {
    start: usize,
    text: &'a str,
}

fn line_ranges(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for segment in source.split_inclusive('\n') {
        let end = start + segment.len();
        lines.push(SourceLine {
            start,
            text: segment.trim(),
        });
        start = end;
    }
    if start < source.len() || source.is_empty() {
        lines.push(SourceLine {
            start,
            text: source[start..].trim(),
        });
    }
    lines
}

fn table_range(
    source: &str,
    target: &str,
    include_marker: bool,
) -> Result<std::ops::Range<usize>, ClspError> {
    let header = format!("[mcp_servers.{target}]");
    let lines = line_ranges(source);
    let matches: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.text == header)
        .map(|(index, _)| index)
        .collect();
    if matches.len() != 1 {
        return Err(setup_error(
            "CLSP MCP table could not be located unambiguously without rewriting unrelated TOML",
        ));
    }
    let header_index = matches[0];
    let start = if include_marker {
        lines[..header_index]
            .iter()
            .rposition(|line| !line.text.is_empty())
            .filter(|index| lines[*index].text == MARKER)
            .map(|index| lines[index].start)
            .ok_or_else(|| setup_error("CLSP marker is detached from its MCP table"))?
    } else {
        lines[header_index].start
    };
    let end = lines
        .iter()
        .skip(header_index + 1)
        .find(|line| line.text.starts_with('[') && line.text.ends_with(']'))
        .map(|line| line.start)
        .unwrap_or(source.len());
    Ok(start..end)
}

fn merge_hooks(source: &str) -> Result<String, ClspError> {
    let mut value: Value = if source.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(source).map_err(setup_error)?
    };
    let root = value
        .as_object_mut()
        .ok_or_else(|| setup_error(".codex/hooks.json must contain a JSON object"))?;
    root.entry("description").or_insert_with(|| {
        json!("CLSP lifecycle, live IDE context, edit safety, and diagnostics for Codex.")
    });
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| setup_error("hooks.json 'hooks' must be an object"))?;
    for groups in hooks.values_mut() {
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| setup_error("each hooks event must contain an array"))?;
        for group in groups.iter_mut() {
            let handlers = group
                .as_object_mut()
                .and_then(|group| group.get_mut("hooks"))
                .and_then(Value::as_array_mut)
                .ok_or_else(|| setup_error("each hook group must contain a hooks array"))?;
            handlers.retain(|handler| !is_owned_hook(handler));
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|handlers| !handlers.is_empty())
        });
    }
    for (event, group) in managed_hook_groups() {
        hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| setup_error("managed hook event is not an array"))?
            .push(group);
    }
    let mut output = serde_json::to_string_pretty(&value).map_err(setup_error)?;
    output.push('\n');
    Ok(output)
}

fn managed_hook_groups() -> [(&'static str, Value); 5] {
    [
        (
            "SessionStart",
            json!({
                "matcher": "startup|resume|clear",
                "hooks": [{
                    "type": "command",
                    "command": "clsp hook session-start",
                    "timeout": 3,
                    "additionalContextLimit": 800,
                    "statusMessage": "Preparing project language servers"
                }]
            }),
        ),
        (
            "UserPromptSubmit",
            json!({
                "hooks": [{
                    "type": "command",
                    "command": "clsp hook user-prompt",
                    "timeout": 1,
                    "additionalContextLimit": 0
                }]
            }),
        ),
        (
            "PreToolUse",
            json!({
                "matcher": "apply_patch|Edit|Write",
                "hooks": [{
                    "type": "command",
                    "command": "clsp hook pre-tool",
                    "timeout": 30,
                    "statusMessage": "Checking unsaved editor changes"
                }]
            }),
        ),
        (
            "PostToolUse",
            json!({
                "matcher": "apply_patch|Edit|Write",
                "hooks": [{
                    "type": "command",
                    "command": "clsp hook post-tool",
                    "timeout": 15,
                    "additionalContextLimit": 1800,
                    "statusMessage": "Checking diagnostics and IDE review"
                }]
            }),
        ),
        (
            "SessionEnd",
            json!({
                "matcher": "other",
                "hooks": [{
                    "type": "command",
                    "command": "clsp hook session-end",
                    "timeout": 2
                }]
            }),
        ),
    ]
}

fn is_owned_hook(handler: &Value) -> bool {
    let Some(command) = handler.get("command").and_then(Value::as_str) else {
        return false;
    };
    let Some(tokens) = command_tokens(command) else {
        return false;
    };
    tokens.len() == 3
        && executable_is_clsp(&tokens[0])
        && tokens[1].eq_ignore_ascii_case("hook")
        && [
            "session-start",
            "user-prompt",
            "pre-tool",
            "post-tool",
            "session-end",
        ]
        .iter()
        .any(|name| tokens[2].eq_ignore_ascii_case(name))
}

fn command_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in command.trim().chars() {
        match character {
            '"' => quoted = !quoted,
            value if value.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            value => current.push(value),
        }
    }
    if quoted {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

fn read_optional_utf8(path: &Path, limit: usize) -> Result<String, ClspError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(setup_error(error)),
    };
    if bytes.len() > limit {
        return Err(setup_error(format!(
            "configuration file exceeds its limit: {}",
            path.display()
        )));
    }
    String::from_utf8(bytes).map_err(setup_error)
}

fn atomic_write(path: &Path, bytes: &[u8], protected: bool) -> Result<(), ClspError> {
    let parent = path
        .parent()
        .ok_or_else(|| setup_error("output path has no parent"))?;
    fs::create_dir_all(parent).map_err(setup_error)?;
    if protected {
        apply_user_system_dacl(parent, true)?;
    }
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{suffix}", std::process::id()));
    fs::write(&temporary, bytes).map_err(setup_error)?;
    if protected {
        apply_user_system_dacl(&temporary, false)?;
    }
    if let Err(error) = atomic_replace(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(setup_error(error));
    }
    if protected {
        verify_user_system_dacl(path)?;
    }
    Ok(())
}

fn write_install_locator(executable: &Path) -> Result<(), ClspError> {
    #[derive(Serialize)]
    struct Locator<'a> {
        executable: &'a Path,
        version: &'static str,
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| setup_error("LOCALAPPDATA is required for CLSP setup"))?;
    let path = PathBuf::from(local).join("clsp").join("install.json");
    let mut bytes = serde_json::to_vec_pretty(&Locator {
        executable,
        version: env!("CARGO_PKG_VERSION"),
    })
    .map_err(setup_error)?;
    bytes.push(b'\n');
    atomic_write(&path, &bytes, true)
}

fn setup_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::InvalidConfig, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn config_merge_is_idempotent_and_preserves_unrelated_tables() {
        let source = "[mcp_servers.other]\ncommand = \"other\"\n";
        let merged = merge_codex_config(source).unwrap();
        assert!(merged.contains("[mcp_servers.other]"));
        assert!(merged.contains(MARKER));
        assert_eq!(merge_codex_config(&merged).unwrap(), merged);
        toml::from_str::<toml::Value>(&merged).unwrap();
    }

    #[test]
    fn config_merge_updates_legacy_clsp_and_rejects_conflicts() {
        let legacy = "[mcp_servers.lsp]\ncommand = \"clsp.exe\"\nargs = []\n";
        let merged = merge_codex_config(legacy).unwrap();
        assert!(merged.contains("[mcp_servers.lsp]"));
        assert!(!merged.contains("[mcp_servers.clsp]"));
        assert!(merge_codex_config("[mcp_servers.clsp]\ncommand = \"other\"\n").is_err());
        assert!(merge_codex_config("[hooks]\n").is_err());
    }

    #[test]
    fn hook_merge_preserves_unrelated_handlers_and_is_idempotent() {
        let source = r#"{"custom":true,"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"other check"},{"type":"command","command":"clsp hook post-tool","timeout":1}]}]}}"#;
        let merged = merge_hooks(source).unwrap();
        assert!(merged.contains("other check"));
        assert!(merged.contains("clsp hook user-prompt"));
        assert!(merged.contains("clsp hook pre-tool"));
        assert_eq!(merge_hooks(&merged).unwrap(), merged);
        assert_eq!(
            serde_json::from_str::<Value>(&merged).unwrap()["custom"],
            true
        );
    }

    #[test]
    fn command_identity_handles_a_quoted_executable_path() {
        let handler = json!({
            "command": "\"C:\\Program Files\\CLSP\\clsp.exe\" hook user-prompt"
        });
        assert!(is_owned_hook(&handler));
    }

    #[test]
    fn code_cli_script_must_be_unique() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("Code.exe");
        fs::write(&executable, []).unwrap();
        let first = directory
            .path()
            .join("version-one/resources/app/out/cli.js");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::write(&first, []).unwrap();
        assert_eq!(
            resolve_code_cli_script(&executable).unwrap(),
            fs::canonicalize(&first).unwrap()
        );

        let second = directory
            .path()
            .join("version-two/resources/app/out/cli.js");
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        fs::write(second, []).unwrap();
        assert!(resolve_code_cli_script(&executable).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn child_process_paths_strip_windows_verbatim_prefixes() {
        assert_eq!(
            child_process_path(Path::new(r"\\?\C:\workspace\extension.vsix")),
            PathBuf::from(r"C:\workspace\extension.vsix")
        );
        assert_eq!(
            child_process_path(Path::new(r"\\?\UNC\server\share\extension.vsix")),
            PathBuf::from(r"\\server\share\extension.vsix")
        );
    }

    #[test]
    fn vsix_identity_and_version_are_checked_from_the_archive() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clsp-ide.vsix");
        let file = fs::File::create(&path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "extension/package.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive
            .write_all(br#"{"publisher":"clsp","name":"clsp-ide","version":"0.1.0"}"#)
            .unwrap();
        archive.finish().unwrap();
        assert!(validate_vsix(&path, "0.1.0").is_ok());
        assert!(validate_vsix(&path, "0.2.0").is_err());
    }
}

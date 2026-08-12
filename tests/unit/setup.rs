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

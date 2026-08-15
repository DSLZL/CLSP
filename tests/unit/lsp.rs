use super::*;

use crate::test_support as support;

fn write_typescript_sdk(root: &Path) -> PathBuf {
    let tsdk = root.join("node_modules").join("typescript").join("lib");
    support::create_dir_all(&tsdk).unwrap();
    support::write(tsdk.join("tsserver.js"), "").unwrap();
    tsdk
}

#[tokio::test]
async fn codec_handles_fragmented_frames() {
    let (mut writer, mut reader) = tokio::io::duplex(256);
    let task = tokio::spawn(async move {
        writer.write_all(b"Content-Len").await.unwrap();
        writer
            .write_all(b"gth: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}")
            .await
            .unwrap();
    });
    let value = read_frame(&mut reader, 128).await.unwrap();
    task.await.unwrap();
    assert_eq!(value["jsonrpc"], "2.0");
}

#[tokio::test]
async fn codec_rejects_oversized_body_before_allocation() {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    writer
        .write_all(b"Content-Length: 999\r\n\r\n")
        .await
        .unwrap();
    assert!(read_frame(&mut reader, 32).await.is_err());
}

#[test]
fn outbound_json_rpc_omits_null_params() {
    for message in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ] {
        assert!(normalize_outgoing_message(message).get("params").is_none());
    }
    assert_eq!(
        normalize_outgoing_message(json!({"jsonrpc": "2.0", "id": 1, "result": null})),
        json!({"jsonrpc": "2.0", "id": 1, "result": null})
    );
}

#[test]
fn converts_unicode_positions_for_all_encodings() {
    let text = "a😀b\n";
    let external = Position { line: 1, column: 3 };
    assert_eq!(
        external_to_lsp(text, external, PositionEncoding::Utf8).unwrap()["character"],
        5
    );
    assert_eq!(
        external_to_lsp(text, external, PositionEncoding::Utf16).unwrap()["character"],
        3
    );
    assert_eq!(
        external_to_lsp(text, external, PositionEncoding::Utf32).unwrap()["character"],
        2
    );
    assert_eq!(
        lsp_to_external(text, 0, 3, PositionEncoding::Utf16).unwrap(),
        external
    );
}

#[tokio::test]
async fn diagnostic_freshness_requires_matching_version() {
    let store = DiagnosticsStore::default();
    let path = PathBuf::from("C:/fixture.rs");
    store.begin_sync(&path, 4).await;
    store.publish(path.clone(), Some(3), Vec::new()).await;
    let report = store
        .report("rust", std::slice::from_ref(&path), Duration::ZERO, 20)
        .await;
    assert!(!report.fresh);
    assert_eq!(
        report.sources[0].reason.as_deref(),
        Some("stale_document_version")
    );

    store.publish(path.clone(), None, Vec::new()).await;
    let report = store
        .report("rust", std::slice::from_ref(&path), Duration::ZERO, 20)
        .await;
    assert!(!report.fresh);
    assert_eq!(
        report.sources[0].reason.as_deref(),
        Some("diagnostic_version_unavailable")
    );

    store.publish(path.clone(), Some(4), Vec::new()).await;
    assert!(
        store
            .report("rust", &[path], Duration::ZERO, 20)
            .await
            .fresh
    );
}

#[tokio::test]
async fn diagnostic_wait_reports_a_stable_timeout_reason() {
    let store = DiagnosticsStore::default();
    let path = PathBuf::from("C:/fixture.rs");
    store.begin_sync(&path, 1).await;
    let report = store.report("rust", &[path], Duration::ZERO, 20).await;
    assert!(!report.fresh);
    assert_eq!(
        report.sources[0].reason.as_deref(),
        Some("diagnostics_timeout")
    );
}

#[tokio::test]
async fn truncated_lsp_diagnostics_are_not_fresh_or_new_errors() {
    let store = DiagnosticsStore::default();
    let path = PathBuf::from("C:/fixture.rs");
    let (baseline, baseline_available) = store.begin_sync(&path, 1).await;
    assert!(!baseline_available);
    store
        .publish_with_truncation(path.clone(), Some(1), Vec::new(), true)
        .await;
    let report = store
        .report("rust", std::slice::from_ref(&path), Duration::ZERO, 20)
        .await;
    assert!(!report.fresh);
    assert_eq!(
        report.sources[0].reason.as_deref(),
        Some("diagnostics_truncated")
    );
    let sync = SyncResult {
        path: path.clone(),
        version: 1,
        baseline,
        baseline_available: true,
    };
    assert!(store.new_errors(&sync).await.is_empty());
}

#[test]
fn astro_initialization_prefers_nearest_project_typescript() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path();
    let root = workspace.join("packages").join("site");
    support::create_dir_all(&root).unwrap();
    write_typescript_sdk(workspace);
    let nearest = write_typescript_sdk(&root);

    let options = server_initialization_options(
        ASTRO_SERVER_ID,
        &root,
        workspace,
        &root.join("node_modules/.bin/astro-ls.cmd"),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        options.pointer("/typescript/tsdk").and_then(Value::as_str),
        Some(nearest.to_string_lossy().as_ref())
    );

    let options = server_initialization_options(
        TYPESCRIPT_SERVER_ID,
        &root,
        workspace,
        &root.join("node_modules/.bin/typescript-language-server.cmd"),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        options.pointer("/tsserver/path").and_then(Value::as_str),
        Some(nearest.join("tsserver.js").to_string_lossy().as_ref())
    );
}

#[test]
fn astro_initialization_uses_manager_typescript() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let root = workspace.join("site");
    support::create_dir_all(&root).unwrap();
    let manager = directory.path().join("manager");
    let installed = write_typescript_sdk(&manager);

    let options = server_initialization_options(
        ASTRO_SERVER_ID,
        &root,
        &workspace,
        &manager.join("bin/astro-ls.cmd"),
        Some(&manager.join("node_modules")),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        options.pointer("/typescript/tsdk").and_then(Value::as_str),
        Some(installed.to_string_lossy().as_ref())
    );

    let options = server_initialization_options(
        TYPESCRIPT_SERVER_ID,
        &root,
        &workspace,
        &manager.join("bin/typescript-language-server.cmd"),
        Some(&manager.join("node_modules")),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        options.pointer("/tsserver/path").and_then(Value::as_str),
        Some(installed.join("tsserver.js").to_string_lossy().as_ref())
    );
}

#[cfg(windows)]
#[test]
fn typescript_initialization_strips_verbatim_sdk_paths_for_node() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let root = workspace.join("site");
    support::create_dir_all(&root).unwrap();
    let manager = directory.path().join("manager");
    let installed = write_typescript_sdk(&manager);
    let modules = std::fs::canonicalize(manager.join("node_modules")).unwrap();

    let options = server_initialization_options(
        TYPESCRIPT_SERVER_ID,
        &root,
        &workspace,
        &manager.join("bin/typescript-language-server.cmd"),
        Some(&modules),
    )
    .unwrap()
    .unwrap();
    let tsserver = options
        .pointer("/tsserver/path")
        .and_then(Value::as_str)
        .unwrap();
    assert!(!tsserver.starts_with(r"\\?\"));
    assert_eq!(
        std::fs::canonicalize(tsserver).unwrap(),
        std::fs::canonicalize(installed.join("tsserver.js")).unwrap()
    );

    let args = server_runtime_args(
        VUE_SERVER_ID,
        &root,
        &workspace,
        &manager.join("bin/vue-language-server.cmd"),
        Some(&modules),
    )
    .unwrap();
    assert_eq!(args.len(), 1);
    assert!(!args[0].starts_with(r"--tsdk=\\?\"));
    assert_eq!(
        std::fs::canonicalize(args[0].strip_prefix("--tsdk=").unwrap()).unwrap(),
        std::fs::canonicalize(installed).unwrap()
    );
}

#[test]
fn only_astro_and_typescript_require_a_typescript_sdk() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let executable = root.join("astro-ls.cmd");
    assert!(
        server_initialization_options("rust", root, root, &executable, None)
            .unwrap()
            .is_none()
    );
    for server_id in [ASTRO_SERVER_ID, TYPESCRIPT_SERVER_ID] {
        let error =
            server_initialization_options(server_id, root, root, &executable, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
        assert!(
            error
                .message
                .contains("requires typescript/lib/tsserver.js")
        );
    }
}

#[test]
fn deno_initialization_enables_the_server() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let options =
        server_initialization_options(DENO_SERVER_ID, root, root, &root.join("deno.exe"), None)
            .unwrap()
            .unwrap();
    assert_eq!(options, json!({"enable": true}));
}

#[test]
fn intelephense_disables_telemetry_and_hosts_only_js_entries_with_node() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let script = root.join("intelephense.js");
    let shim = root.join("intelephense.cmd");
    let options = server_initialization_options(INTELEPHENSE_SERVER_ID, root, root, &script, None)
        .unwrap()
        .unwrap();
    assert_eq!(options, json!({"telemetry": {"enabled": false}}));
    assert!(uses_node_host(INTELEPHENSE_SERVER_ID, &script));
    assert!(!uses_node_host(INTELEPHENSE_SERVER_ID, &shim));
    assert!(uses_node_host(ESLINT_SERVER_ID, &script));
    assert!(!uses_node_host("typescript", &script));
}

#[test]
fn prisma_has_no_custom_initialization_and_hosts_only_js_entries_with_node() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let script = root.join("bin.js");
    let shim = root.join("prisma-language-server.cmd");
    assert!(
        server_initialization_options(PRISMA_SERVER_ID, root, root, &script, None)
            .unwrap()
            .is_none()
    );
    assert!(uses_node_host(PRISMA_SERVER_ID, &script));
    assert!(!uses_node_host(PRISMA_SERVER_ID, &shim));
    assert_eq!(diagnostic_version(PRISMA_SERVER_ID, None, Some(2)), Some(2));
    assert_eq!(
        diagnostic_version(PRISMA_SERVER_ID, Some(1), Some(2)),
        Some(1)
    );
}

#[test]
fn pyright_has_no_custom_initialization_and_hosts_only_js_entries_with_node() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let script = root.join("server.js");
    let shim = root.join("pyright-langserver.cmd");
    assert!(
        server_initialization_options(PYRIGHT_SERVER_ID, root, root, &script, None)
            .unwrap()
            .is_none()
    );
    assert!(uses_node_host(PYRIGHT_SERVER_ID, &script));
    assert!(!uses_node_host(PYRIGHT_SERVER_ID, &shim));
}

#[test]
fn terraform_initialization_enables_validation_features() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let options = server_initialization_options(
        TERRAFORM_SERVER_ID,
        root,
        root,
        &root.join("terraform-ls.exe"),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        options,
        json!({
            "experimentalFeatures": {
                "prefillRequiredFields": true,
                "validateOnSave": true
            }
        })
    );
}

#[test]
fn svelte_has_no_custom_initialization_and_hosts_js_entries_with_node() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let script = root.join("server.js");
    let shim = root.join("svelteserver.cmd");
    assert!(
        server_initialization_options(SVELTE_SERVER_ID, root, root, &script, None)
            .unwrap()
            .is_none()
    );
    assert!(uses_node_host(SVELTE_SERVER_ID, &script));
    assert!(!uses_node_host(SVELTE_SERVER_ID, &shim));
}

#[test]
fn yaml_adds_l10n_only_for_extension_js_and_hosts_it_with_node() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let script = root.join("redhat.vscode-yaml-1.24.0/dist/languageserver.js");
    let shim = root.join("yaml-language-server.cmd");
    support::create_dir_all(script.parent().unwrap().join("l10n")).unwrap();
    support::write(&script, b"server").unwrap();
    support::write(
        root.join("redhat.vscode-yaml-1.24.0/dist/l10n/bundle.l10n.json"),
        b"{}",
    )
    .unwrap();
    let l10n = std::fs::canonicalize(root.join("redhat.vscode-yaml-1.24.0/dist/l10n")).unwrap();
    assert_eq!(
        server_initialization_options(YAML_LS_SERVER_ID, root, root, &script, None).unwrap(),
        Some(json!({"l10nPath": child_process_path(&l10n).to_string_lossy()}))
    );
    assert!(
        server_initialization_options(YAML_LS_SERVER_ID, root, root, &shim, None)
            .unwrap()
            .is_none()
    );
    assert!(uses_node_host(YAML_LS_SERVER_ID, &script));
    assert!(!uses_node_host(YAML_LS_SERVER_ID, &shim));
}

#[test]
fn vue_uses_a_verified_typescript_sdk_runtime_arg_and_no_custom_initialization() {
    let directory = support::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let root = workspace.join("site");
    support::create_dir_all(&root).unwrap();
    let manager = directory.path().join("manager");
    let tsdk = write_typescript_sdk(&manager);
    let script = root.join("language-server.js");
    let shim = root.join("vue-language-server.cmd");

    assert!(
        server_initialization_options(VUE_SERVER_ID, &root, &workspace, &script, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        server_runtime_args(
            VUE_SERVER_ID,
            &root,
            &workspace,
            &shim,
            Some(&manager.join("node_modules")),
        )
        .unwrap(),
        [format!("--tsdk={}", tsdk.to_string_lossy())]
    );
    assert!(uses_node_host(VUE_SERVER_ID, &script));
    assert!(!uses_node_host(VUE_SERVER_ID, &shim));
    assert!(
        server_runtime_args("rust", &root, &workspace, &shim, None)
            .unwrap()
            .is_empty()
    );

    let error = server_runtime_args(VUE_SERVER_ID, &root, &workspace, &shim, None).unwrap_err();
    assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
    assert!(
        error
            .message
            .contains("requires typescript/lib/tsserver.js")
    );
}

#[test]
fn vue_tsserver_notifications_use_the_bounded_standalone_fallback() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    support::write(root.join("jsconfig.json"), "{}").unwrap();
    let request = json!([[7, "_vue:projectInfo", {"file": "App.vue"}]]);
    let response =
        server_notification_response(VUE_SERVER_ID, "tsserver/request", Some(&request), root)
            .unwrap();
    assert_eq!(response["method"], "tsserver/response");
    assert_eq!(response["params"][0][0], 7);
    assert_eq!(
        Path::new(response["params"][0][1]["configFileName"].as_str().unwrap()),
        root.join("jsconfig.json")
    );

    support::write(root.join("tsconfig.json"), "{}").unwrap();
    let response =
        server_notification_response(VUE_SERVER_ID, "tsserver/request", Some(&request), root)
            .unwrap();
    assert_eq!(
        Path::new(response["params"][0][1]["configFileName"].as_str().unwrap()),
        root.join("tsconfig.json")
    );
    std::fs::remove_file(root.join("tsconfig.json")).unwrap();
    std::fs::remove_file(root.join("jsconfig.json")).unwrap();
    let response =
        server_notification_response(VUE_SERVER_ID, "tsserver/request", Some(&request), root)
            .unwrap();
    assert_eq!(
        Path::new(response["params"][0][1]["configFileName"].as_str().unwrap()),
        root.join("tsconfig.json")
    );

    let request = json!([[8, "_vue:getComponentMeta", {}]]);
    let response =
        server_notification_response(VUE_SERVER_ID, "tsserver/request", Some(&request), root)
            .unwrap();
    assert_eq!(response["params"], json!([[8, null]]));
    assert!(
        server_notification_response("rust", "tsserver/request", Some(&request), root).is_none()
    );
    assert!(
        server_notification_response(VUE_SERVER_ID, "tsserver/request", Some(&json!([])), root)
            .is_none()
    );
}

#[test]
fn fsharp_initialization_and_dll_host_are_explicit() {
    let directory = support::tempdir().unwrap();
    let root = directory.path();
    let dll = root.join("fsautocomplete.DLL");
    let options = server_initialization_options(FSHARP_SERVER_ID, root, root, &dll, None)
        .unwrap()
        .unwrap();
    assert_eq!(options, json!({"AutomaticWorkspaceInit": true}));
    assert!(uses_dotnet_host(FSHARP_SERVER_ID, &dll));
    assert!(!uses_dotnet_host("csharp", &dll));
    assert!(!uses_dotnet_host(
        FSHARP_SERVER_ID,
        &root.join("fsautocomplete.exe")
    ));
    assert_eq!(strip_utf8_bom("\u{feff}let value = 1"), "let value = 1");
    assert_eq!(strip_utf8_bom("let value = 1"), "let value = 1");
    assert_eq!(diagnostic_version(FSHARP_SERVER_ID, None, Some(2)), Some(2));
    assert_eq!(diagnostic_version("rust", None, Some(2)), None);
    assert_eq!(
        diagnostic_version(FSHARP_SERVER_ID, Some(1), Some(2)),
        Some(1)
    );
}

#[test]
fn jdtls_initialization_names_the_server_root() {
    let directory = support::tempdir().unwrap();
    let root = directory.path().join("java-project");
    support::create_dir(&root).unwrap();
    let options = server_initialization_options(
        JDTLS_SERVER_ID,
        &root,
        directory.path(),
        &root.join("jdtls.cmd"),
        None,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        options,
        json!({"workspaceFolders": [path_to_uri(&root).unwrap()], "settings": {}})
    );
    assert_eq!(
        server_request_timeout(JDTLS_SERVER_ID, Duration::from_secs(10)),
        SLOW_REQUEST_TIMEOUT
    );
    assert_eq!(
        server_request_timeout(JULIALS_SERVER_ID, Duration::from_secs(10)),
        SLOW_REQUEST_TIMEOUT
    );
    assert_eq!(
        server_request_timeout("rust", Duration::from_secs(10)),
        Duration::from_secs(10)
    );
}

#[test]
fn ruby_lsp_uses_slow_initialize_without_custom_options() {
    let root = Path::new("C:/ruby-project");
    assert_eq!(
        initialization_timeout(RUBY_LSP_SERVER_ID, Duration::from_secs(10)),
        SLOW_INITIALIZE_TIMEOUT
    );
    assert!(
        server_initialization_options(
            RUBY_LSP_SERVER_ID,
            root,
            root,
            &root.join("ruby-lsp.bat"),
            None,
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn sourcekit_lsp_uses_slow_initialize_without_custom_options() {
    let root = Path::new("C:/swift-project");
    assert_eq!(
        initialization_timeout(SOURCEKIT_LSP_SERVER_ID, Duration::from_secs(10)),
        SLOW_INITIALIZE_TIMEOUT
    );
    assert!(
        server_initialization_options(
            SOURCEKIT_LSP_SERVER_ID,
            root,
            root,
            &root.join("sourcekit-lsp.exe"),
            None,
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        server_request_timeout(SOURCEKIT_LSP_SERVER_ID, Duration::from_secs(10)),
        Duration::from_secs(10)
    );
}

#[test]
fn jdtls_extension_uses_the_official_java_launcher_arguments() {
    let root = Path::new("C:/extension/server");
    let configuration = root.join("config_win");
    let launcher = root
        .join("plugins")
        .join("org.eclipse.equinox.launcher_1.7.0.jar");
    assert!(uses_jdtls_java_host(JDTLS_SERVER_ID, &launcher));
    assert!(!uses_jdtls_java_host(
        JDTLS_SERVER_ID,
        Path::new("C:/bin/jdtls.cmd")
    ));
    assert!(!uses_jdtls_java_host("rust", &launcher));

    let args = jdtls_vm_args(&configuration, &launcher, 21);
    assert_eq!(args[0], "-Declipse.application=org.eclipse.jdt.ls.core.id1");
    assert!(args.contains(&"-Dosgi.sharedConfiguration.area.readOnly=true".into()));
    assert!(args.contains(&"-Dosgi.configuration.cascaded=true".into()));
    assert!(args.contains(&"-Xms1G".into()));
    assert_eq!(args[args.len() - 2], "-jar");
    assert_eq!(args.last().unwrap(), &launcher.to_string_lossy());
    assert!(
        !args
            .iter()
            .any(|arg| arg.starts_with("-Djdk.xml.maxGeneralEntitySizeLimit"))
    );

    let java_24_args = jdtls_vm_args(&configuration, &launcher, 24);
    assert_eq!(java_24_args[0], "-Djdk.xml.maxGeneralEntitySizeLimit=0");
    assert_eq!(java_24_args[1], "-Djdk.xml.totalEntitySizeLimit=0");
}

#[test]
fn julials_extension_uses_its_project_without_initialization_options() {
    let julia = Path::new("C:/Julia/bin/julia.exe");
    let environment = Path::new("C:/extensions/julia/scripts/environments/languageserver/v1.11");
    let command = julials_command(julia, environment);
    assert_eq!(command.as_std().get_program(), julia.as_os_str());
    assert_eq!(
        command.as_std().get_args().collect::<Vec<_>>(),
        [std::ffi::OsString::from(format!(
            "--project={}",
            environment.to_string_lossy()
        ))]
    );
    assert!(
        server_initialization_options(JULIALS_SERVER_ID, environment, environment, julia, None,)
            .unwrap()
            .is_none()
    );
}

#[cfg(windows)]
#[test]
fn fsharp_diagnostic_uri_round_trips_an_encoded_lowercase_drive() {
    let directory = support::tempdir().unwrap();
    let file = directory.path().join("Program.fs");
    support::write(&file, "module Demo").unwrap();
    let workspace = Workspace::open(directory.path()).unwrap();
    let canonical = std::fs::canonicalize(&file).unwrap();
    assert!(!path_to_uri(workspace.root()).unwrap().contains("%3F"));
    let ordinary = canonical
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    let (drive, rest) = ordinary.split_once(':').unwrap();
    let uri = format!("file:///{}%3A{rest}", drive.to_ascii_lowercase());

    let raw = uri_to_path(&uri).unwrap();
    assert_eq!(workspace.resolve_file(raw, 1024).unwrap(), canonical);
}

#[test]
fn server_configuration_is_scoped_to_eslint_and_prisma() {
    let params = json!({"items": [{}, {}]});
    let eslint = server_request_response(
        ESLINT_SERVER_ID,
        "workspace/configuration",
        Some(&params),
        "file:///workspace",
        "workspace",
    )
    .unwrap();
    assert_eq!(
        eslint,
        json!([
            {
                "validate": "on",
                "workspaceFolder": {"uri": "file:///workspace", "name": "workspace"}
            },
            {
                "validate": "on",
                "workspaceFolder": {"uri": "file:///workspace", "name": "workspace"}
            }
        ])
    );
    assert_eq!(
        server_request_response(
            PRISMA_SERVER_ID,
            "workspace/configuration",
            Some(&params),
            "file:///workspace",
            "workspace",
        )
        .unwrap(),
        json!([{}, {}])
    );
    assert_eq!(
        server_request_response(
            "rust",
            "workspace/configuration",
            Some(&params),
            "file:///workspace",
            "workspace",
        )
        .unwrap(),
        json!([null, null])
    );
    for method in [
        "eslint/noConfig",
        "eslint/noLibrary",
        "eslint/openDoc",
        "eslint/probeFailed",
    ] {
        assert_eq!(
            server_request_response(
                ESLINT_SERVER_ID,
                method,
                None,
                "file:///workspace",
                "workspace",
            )
            .unwrap(),
            Value::Null
        );
    }
}

#[test]
fn diagnostic_refresh_request_is_acknowledged() {
    assert_eq!(
        server_request_response(
            KOTLIN_LS_SERVER_ID,
            "workspace/diagnostic/refresh",
            None,
            "file:///workspace",
            "workspace",
        )
        .unwrap(),
        Value::Null
    );
}

#[test]
fn only_slow_starting_servers_get_the_long_initialize_timeout() {
    let normal = Duration::from_secs(10);
    for server_id in [
        CLOJURE_SERVER_ID,
        ELIXIR_LS_SERVER_ID,
        JULIALS_SERVER_ID,
        KOTLIN_LS_SERVER_ID,
        SOURCEKIT_LSP_SERVER_ID,
    ] {
        assert_eq!(
            initialization_timeout(server_id, normal),
            Duration::from_secs(300)
        );
    }
    assert_eq!(initialization_timeout("rust", normal), normal);
}

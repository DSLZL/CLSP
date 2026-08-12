use super::*;

use crate::test_support as support;

#[test]
fn client_context_enforces_bootstrap_contract() {
    let directory = support::tempdir().unwrap();
    let workspace_root = directory.path().join("workspace");
    support::create_dir(&workspace_root).unwrap();
    support::write(workspace_root.join(".clsp.toml"), "enabled = true\n").unwrap();
    let outside = directory.path().join("outside.rs");
    support::write(&outside, "").unwrap();

    let context = ClientContext::open(&workspace_root).unwrap();
    assert!(context.config.enabled);
    assert_eq!(
        context.workspace.root(),
        fs::canonicalize(&workspace_root).unwrap()
    );
    assert_eq!(
        context
            .workspace
            .resolve_file(&outside, context.config.limits.max_file_bytes)
            .unwrap_err()
            .code,
        ErrorCode::PathOutsideWorkspace
    );

    match std::env::var_os("LOCALAPPDATA") {
        Some(local_app_data) => {
            let state = PathBuf::from(local_app_data)
                .join("clsp/state/workspaces")
                .join(context.workspace.hash());
            assert!(!state.exists());
            let connector = context.connector(ClientKind::Status).unwrap();
            assert_eq!(connector.workspace, context.workspace.root());
            assert_eq!(connector.metadata_path, state.join("broker.json"));
            assert!(state.join("logs").is_dir());
            fs::remove_dir_all(&state).unwrap();
        }
        None => {
            assert_eq!(
                context.connector(ClientKind::Status).unwrap_err().code,
                ErrorCode::InvalidConfig
            );
        }
    }

    let disabled = directory.path().join("disabled");
    support::create_dir(&disabled).unwrap();
    support::write(disabled.join(".clsp.toml"), "enabled = false\n").unwrap();
    assert!(
        ClientContext::open(&disabled)
            .err()
            .unwrap()
            .message
            .contains("disabled")
    );

    let invalid = directory.path().join("invalid");
    support::create_dir(&invalid).unwrap();
    support::write(invalid.join(".clsp.toml"), "unknown = true\n").unwrap();
    assert_eq!(
        ClientContext::open(&invalid).err().unwrap().code,
        ErrorCode::InvalidConfig
    );
}

#[tokio::test]
async fn wire_round_trip_uses_bounded_big_endian_framing() {
    let (mut writer, mut reader) = tokio::io::duplex(512);
    let message = WireMessage::Request(RequestEnvelope {
        id: 9,
        body: RpcRequest::Discover,
    });
    let expected = message.clone();
    let task = tokio::spawn(async move { write_wire(&mut writer, &message, 1_024).await });
    assert_eq!(read_wire(&mut reader, 1_024).await.unwrap(), expected);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn authentication_handshake_is_bounded() {
    let pipe_name = format!(
        r"\\.\pipe\clsp-handshake-test-{}-{}",
        std::process::id(),
        now_ms()
    );
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)
        .unwrap();
    let server_task = tokio::spawn(async move {
        server.connect().await.unwrap();
        std::future::pending::<()>().await;
    });
    let connector = BrokerConnector {
        workspace: PathBuf::from("C:/fixture"),
        metadata_path: PathBuf::from("C:/fixture/broker.json"),
        max_frame_bytes: 1_024,
        client_kind: ClientKind::Status,
    };
    let metadata = BrokerMetadata {
        protocol: PROTOCOL_VERSION.into(),
        pipe_name,
        pid: std::process::id(),
        started_ms: now_ms(),
        token: "test-token".into(),
        workspace_root: PathBuf::from("C:/fixture"),
    };

    let error = connector
        .connect_authenticated_bounded(&metadata, Duration::from_millis(50))
        .await
        .unwrap_err();
    server_task.abort();
    assert_eq!(error.code, ErrorCode::BrokerUnavailable);
    assert!(error.message.contains("timed out"));
}

#[test]
fn auth_comparison_is_length_sensitive() {
    assert!(constant_time_eq(b"same", b"same"));
    assert!(!constant_time_eq(b"same", b"diff"));
    assert!(!constant_time_eq(b"same", b"same-longer"));
}

#[test]
fn ide_route_never_falls_back_from_an_explicit_session() {
    let first = "a".repeat(IDE_SESSION_ID_HEX_LEN);
    let second = "b".repeat(IDE_SESSION_ID_HEX_LEN);
    let mut candidates = BTreeMap::new();
    candidates.insert(first.clone(), ());
    assert_eq!(
        choose_ide_session(&candidates, Some(&second), true),
        Err(RouteChoice::InvalidHint)
    );
    assert_eq!(
        choose_ide_session(&candidates, Some(&first), false),
        Ok(first.as_str())
    );
}

#[test]
fn ide_route_requires_complete_unique_discovery_without_a_hint() {
    let session = "c".repeat(IDE_SESSION_ID_HEX_LEN);
    let mut candidates = BTreeMap::new();
    candidates.insert(session.clone(), ());
    assert_eq!(
        choose_ide_session(&candidates, None, false),
        Err(RouteChoice::Unavailable)
    );
    assert_eq!(
        choose_ide_session(&candidates, None, true),
        Ok(session.as_str())
    );
    candidates.insert("d".repeat(IDE_SESSION_ID_HEX_LEN), ());
    assert_eq!(
        choose_ide_session(&candidates, None, true),
        Err(RouteChoice::Ambiguous)
    );
}

#[test]
fn duplicate_session_prefers_the_deeper_workspace() {
    assert!(prefer_workspace(
        Path::new(r"C:\repo"),
        Path::new(r"C:\repo\nested")
    ));
    assert!(!prefer_workspace(
        Path::new(r"C:\repo\nested"),
        Path::new(r"C:\repo")
    ));
}

#[test]
fn protected_acl_round_trip_has_only_user_and_system() {
    let directory = support::tempdir().unwrap();
    let file = directory.path().join("broker.json");
    support::write(&file, b"{}").unwrap();
    apply_user_system_dacl(&file, false).unwrap();
    verify_user_system_dacl(&file).unwrap();

    let inherited = directory.path().join("state");
    support::create_dir(&inherited).unwrap();
    apply_user_system_dacl(&inherited, true).unwrap();
    verify_user_system_dacl(&inherited).unwrap();
}

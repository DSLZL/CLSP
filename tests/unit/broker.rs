use super::*;

fn test_broker() -> (tempfile::TempDir, Arc<Broker>, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let workspace_root = directory.path().join("workspace");
    let state = directory.path().join("state");
    let paths = StatePaths {
        logs: state.join("logs"),
        artifacts: directory.path().join("artifacts"),
        workspace_state: state,
    };
    fs::create_dir_all(&workspace_root).unwrap();
    for path in [&paths.logs, &paths.workspace_state, &paths.artifacts] {
        fs::create_dir_all(path).unwrap();
    }
    let workspace = Workspace::open(&workspace_root).unwrap();
    let mut config = Config::default();
    config.diagnostics.wait_ms = 0;
    let broker = Broker::new(config, workspace, Registry::builtin().unwrap(), paths).unwrap();
    (directory, broker, workspace_root)
}

async fn register_test_ide(broker: &Arc<Broker>, root: &Path, session_id: &str) {
    broker
        .handle(RpcRequest::RegisterIde {
            session_id: session_id.into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
            workspace_root: root.to_path_buf(),
        })
        .await
        .unwrap();
}

async fn next_ide_action(broker: &Arc<Broker>, session_id: &str) -> IdeActionEnvelope {
    let RpcResponse::IdeAction {
        action: Some(action),
    } = broker
        .handle(RpcRequest::PollIdeActions {
            session_id: session_id.into(),
            wait_ms: 500,
        })
        .await
        .unwrap()
    else {
        panic!("IDE action was not queued");
    };
    action
}

async fn complete_ide_action(
    broker: &Arc<Broker>,
    session_id: &str,
    action_id: u64,
    result: IdeActionResult,
) {
    broker
        .handle(RpcRequest::CompleteIdeAction {
            session_id: session_id.into(),
            action_id,
            result,
        })
        .await
        .unwrap();
}

fn ide_error(path: &Path, message: &str, start: u32, end: u32) -> IdeDiagnostic {
    IdeDiagnostic {
        path: path.to_path_buf(),
        range: crate::protocol::IdeRange {
            start: crate::protocol::IdePosition {
                line: 0,
                character: start,
            },
            end: crate::protocol::IdePosition {
                line: 0,
                character: end,
            },
        },
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        source: Some("rust-analyzer".into()),
        code: Some(message.into()),
    }
}

async fn prepare_with_baseline(
    broker: &Arc<Broker>,
    session_id: &str,
    file: &Path,
    tool_use_id: &str,
    diagnostics: Vec<IdeDiagnostic>,
    truncated: bool,
) {
    let request = {
        let broker = Arc::clone(broker);
        let session_id = session_id.to_owned();
        let file = file.to_path_buf();
        let tool_use_id = tool_use_id.to_owned();
        tokio::spawn(async move {
            broker
                .handle(RpcRequest::PrepareEdit {
                    session_id,
                    codex_session_id: "codex-session".into(),
                    tool_use_id,
                    targets: vec![EditTarget {
                        kind: EditKind::Update,
                        path: file,
                        move_to: None,
                    }],
                })
                .await
        })
    };
    let action = next_ide_action(broker, session_id).await;
    assert!(matches!(action.action, IdeAction::PrepareEdit { .. }));
    complete_ide_action(
        broker,
        session_id,
        action.action_id,
        IdeActionResult::Prepared {
            outcome: IdePrepareOutcome::Ready,
            message: None,
        },
    )
    .await;
    let action = next_ide_action(broker, session_id).await;
    assert!(matches!(action.action, IdeAction::GetDiagnostics { .. }));
    complete_ide_action(
        broker,
        session_id,
        action.action_id,
        IdeActionResult::Diagnostics {
            items: diagnostics,
            truncated,
        },
    )
    .await;
    assert!(matches!(
        request.await.unwrap().unwrap(),
        RpcResponse::IdePrepared { .. }
    ));
}

async fn sync_with_ide_diagnostics(
    broker: &Arc<Broker>,
    session_id: &str,
    file: &Path,
    tool_use_id: &str,
    diagnostics: Vec<IdeDiagnostic>,
) -> RpcResponse {
    let request = {
        let broker = Arc::clone(broker);
        let session_id = session_id.to_owned();
        let file = file.to_path_buf();
        let tool_use_id = tool_use_id.to_owned();
        tokio::spawn(async move {
            broker
                .handle(RpcRequest::SyncIdeDiagnostics {
                    session_id,
                    codex_session_id: "codex-session".into(),
                    tool_use_id,
                    paths: vec![file],
                })
                .await
        })
    };
    let action = next_ide_action(broker, session_id).await;
    assert!(matches!(action.action, IdeAction::GetDiagnostics { .. }));
    complete_ide_action(
        broker,
        session_id,
        action.action_id,
        IdeActionResult::Diagnostics {
            items: diagnostics,
            truncated: false,
        },
    )
    .await;
    request.await.unwrap().unwrap()
}

#[test]
fn fsharp_start_args_use_a_root_specific_state_directory() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    let root = directory.path().join("workspace/project");
    let args = lsp_start_args(FSHARP_SERVER_ID, &["existing".into()], &root, &state).unwrap();
    assert_eq!(args[0], "existing");
    assert_eq!(args[1], "--state-directory");
    let state_directory = PathBuf::from(&args[2]);
    assert!(state_directory.is_dir());
    assert!(state_directory.starts_with(state.join("lsp/fsharp")));
    assert_ne!(
        args,
        lsp_start_args(
            FSHARP_SERVER_ID,
            &["existing".into()],
            &directory.path().join("workspace/other"),
            &state,
        )
        .unwrap()
    );
    assert_eq!(
        lsp_start_args("rust", &["--stdio".into()], &root, &state).unwrap(),
        ["--stdio"]
    );
}

#[test]
fn jdtls_start_args_use_a_root_specific_data_directory() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    let root = directory.path().join("workspace/project");
    let args = lsp_start_args(JDTLS_SERVER_ID, &[], &root, &state).unwrap();
    assert_eq!(args[0], "-data");
    let data = PathBuf::from(&args[1]);
    assert!(data.is_dir());
    assert!(data.starts_with(state.join("lsp/jdtls")));
    assert_ne!(
        args,
        lsp_start_args(
            JDTLS_SERVER_ID,
            &[],
            &directory.path().join("workspace/other"),
            &state,
        )
        .unwrap()
    );
}

#[test]
fn kotlin_start_args_use_a_root_specific_system_path() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    let root = directory.path().join("workspace/project");
    let args = lsp_start_args(KOTLIN_LS_SERVER_ID, &["--stdio".into()], &root, &state).unwrap();
    assert_eq!(args[0], "--stdio");
    assert_eq!(args[1], "--system-path");
    let system_path = PathBuf::from(&args[2]);
    assert!(system_path.is_dir());
    assert!(system_path.starts_with(state.join("lsp/kotlin-ls")));
    assert_ne!(
        args,
        lsp_start_args(
            KOTLIN_LS_SERVER_ID,
            &["--stdio".into()],
            &directory.path().join("workspace/other"),
            &state,
        )
        .unwrap()
    );
}

#[tokio::test]
async fn expired_leases_do_not_require_session_end() {
    let now = Instant::now();
    let leases = LeaseRuntime::new(Duration::from_secs(10));
    leases.renew("alive".into(), now).await;
    leases
        .renew(
            "expired".into(),
            now.checked_sub(Duration::from_secs(10)).unwrap(),
        )
        .await;
    let expired = leases.sweep(now).await;
    assert_eq!(expired, vec!["expired"]);
    assert_eq!(leases.active_count().await, 1);
}

#[tokio::test]
async fn lease_lifecycle_is_visible_through_broker_interface() {
    let (_directory, broker, _) = test_broker();
    assert!(matches!(
        broker
            .handle(RpcRequest::RenewLease {
                session_id: "session-123".into(),
            })
            .await
            .unwrap(),
        RpcResponse::Ack
    ));
    let RpcResponse::Snapshot(snapshot) = broker.handle(RpcRequest::Snapshot).await.unwrap() else {
        panic!("snapshot response expected");
    };
    assert_eq!(snapshot.active_leases, 1);

    broker
        .handle(RpcRequest::ReleaseLease {
            session_id: "session-123".into(),
        })
        .await
        .unwrap();
    let RpcResponse::Events { events } = broker
        .handle(RpcRequest::Subscribe { after_seq: 0 })
        .await
        .unwrap()
    else {
        panic!("events response expected");
    };
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0].body,
        EventBody::LeaseChanged {
            session_id,
            active: true,
        } if session_id == "session-123"
    ));
    assert!(matches!(
        &events[1].body,
        EventBody::LeaseChanged {
            session_id,
            active: false,
        } if session_id == "session-123"
    ));
}

#[test]
fn session_ids_are_bounded_before_state_mutation() {
    assert!(validate_session_id("session-123").is_ok());
    assert!(validate_session_id("").is_err());
    assert!(validate_session_id("contains space").is_err());
    assert!(validate_session_id(&"a".repeat(129)).is_err());
}

#[test]
fn watcher_baseline_is_consumed_once_by_the_hook() {
    let client_key = ClientKey {
        root: "C:/workspace".into(),
        server_id: "rust".into(),
        artifact_version: "1".into(),
        config_digest: "digest".into(),
    };
    let path = PathBuf::from("C:/workspace/src/lib.rs");
    let sync = SyncResult {
        path: path.clone(),
        version: 2,
        baseline: BTreeSet::from(["old-error".into()]),
        baseline_available: true,
    };
    let key = (client_key, path);
    let mut baselines = BTreeMap::new();

    assert!(handoff_watcher_baseline(&mut baselines, key.clone(), Some(sync), true, 8).is_none());
    let consumed = handoff_watcher_baseline(&mut baselines, key.clone(), None, false, 8).unwrap();
    assert_eq!(consumed.version, 2);
    assert!(handoff_watcher_baseline(&mut baselines, key, None, false, 8).is_none());
}

#[test]
fn restart_backoff_is_exponential_and_bounded() {
    assert_eq!(retry_delay(1), 1);
    assert_eq!(retry_delay(2), 2);
    assert_eq!(retry_delay(6), 32);
    assert_eq!(retry_delay(100), 32);
}

#[test]
fn ide_session_ids_are_exact_random_hex_tokens() {
    assert!(validate_ide_session_id(&"a".repeat(IDE_SESSION_ID_HEX_LEN)).is_ok());
    assert!(validate_ide_session_id("session-1").is_err());
    assert!(validate_ide_session_id(&"g".repeat(IDE_SESSION_ID_HEX_LEN)).is_err());
}

#[test]
fn ide_registry_rejects_full_queues_and_expires_sessions() {
    let now = Instant::now();
    let session_id = "a".repeat(IDE_SESSION_ID_HEX_LEN);
    let mut registry = IdeRegistry::default();
    registry.sessions.insert(
        session_id.clone(),
        IdeSession {
            adapter_version: env!("CARGO_PKG_VERSION").into(),
            workspace_root: PathBuf::from("C:/workspace"),
            last_seen: now,
            queue: VecDeque::new(),
            pending: HashMap::new(),
            diagnostic_baselines: BaselineStore::new(IDE_DIAGNOSTIC_BASELINE_CAPACITY),
            notify: Arc::new(Notify::new()),
        },
    );
    for _ in 0..IDE_ACTION_QUEUE_CAPACITY {
        registry
            .enqueue(&session_id, IdeAction::GetEditorContext {}, now)
            .unwrap();
    }
    assert_eq!(
        registry
            .enqueue(&session_id, IdeAction::GetEditorContext {}, now)
            .unwrap_err()
            .code,
        ErrorCode::IdeUnavailable
    );
    registry.sweep(now + IDE_SESSION_TTL + Duration::from_millis(1));
    assert!(registry.sessions.is_empty());
}

#[tokio::test]
async fn watcher_reuse_requires_a_live_ide_session() {
    let (_directory, broker, root) = test_broker();
    assert!(!broker.has_live_ide_session().await);
    let session_id = "e".repeat(IDE_SESSION_ID_HEX_LEN);
    register_test_ide(&broker, &root, &session_id).await;
    assert!(broker.has_live_ide_session().await);

    broker
        .ide
        .lock()
        .await
        .sessions
        .get_mut(&session_id)
        .unwrap()
        .last_seen = Instant::now() - IDE_SESSION_TTL - Duration::from_millis(1);
    assert!(!broker.has_live_ide_session().await);
}

#[tokio::test]
async fn ide_problems_baseline_reports_only_new_errors() {
    let (_directory, broker, root) = test_broker();
    let file = root.join("main.rs");
    fs::write(&file, "a😀b\n").unwrap();
    let session_id = "c".repeat(IDE_SESSION_ID_HEX_LEN);
    register_test_ide(&broker, &root, &session_id).await;
    let old = ide_error(&file, "old", 0, 1);

    prepare_with_baseline(
        &broker,
        &session_id,
        &file,
        "no-new-error",
        vec![old.clone()],
        false,
    )
    .await;
    let RpcResponse::Sync {
        new_errors,
        fresh,
        baseline_available,
        ..
    } = sync_with_ide_diagnostics(
        &broker,
        &session_id,
        &file,
        "no-new-error",
        vec![old.clone()],
    )
    .await
    else {
        panic!("IDE diagnostics sync response was not returned");
    };
    assert!(new_errors.is_empty());
    assert!(fresh && baseline_available);

    prepare_with_baseline(
        &broker,
        &session_id,
        &file,
        "new-error",
        vec![old.clone()],
        false,
    )
    .await;
    let new = ide_error(&file, "new", 3, 4);
    let RpcResponse::Sync {
        new_errors,
        fresh,
        baseline_available,
        ..
    } = sync_with_ide_diagnostics(&broker, &session_id, &file, "new-error", vec![old, new]).await
    else {
        panic!("IDE diagnostics sync response was not returned");
    };
    assert!(fresh && baseline_available);
    assert_eq!(new_errors.len(), 1);
    assert_eq!(new_errors[0].message, "new");
    assert_eq!(new_errors[0].range.start.column, 3);
    assert_eq!(new_errors[0].server_id, "vscode_problems");
}

#[tokio::test]
async fn truncated_ide_baseline_is_never_injected() {
    let (_directory, broker, root) = test_broker();
    let file = root.join("main.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    let session_id = "d".repeat(IDE_SESSION_ID_HEX_LEN);
    register_test_ide(&broker, &root, &session_id).await;
    prepare_with_baseline(
        &broker,
        &session_id,
        &file,
        "truncated",
        vec![ide_error(&file, "old", 0, 1)],
        true,
    )
    .await;

    let RpcResponse::Sync {
        new_errors,
        fresh,
        baseline_available,
        ..
    } = broker
        .handle(RpcRequest::SyncIdeDiagnostics {
            session_id,
            codex_session_id: "codex-session".into(),
            tool_use_id: "truncated".into(),
            paths: vec![file],
        })
        .await
        .unwrap()
    else {
        panic!("IDE diagnostics sync response was not returned");
    };
    assert!(new_errors.is_empty());
    assert!(!fresh && !baseline_available);
}

#[tokio::test]
async fn ide_payload_round_trip_never_enters_public_events() {
    let (_directory, broker, root) = test_broker();
    let file = root.join("main.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    let session_id = "a".repeat(IDE_SESSION_ID_HEX_LEN);
    register_test_ide(&broker, &root, &session_id).await;

    let request = {
        let broker = Arc::clone(&broker);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            broker
                .handle(RpcRequest::GetIdeContext { session_id })
                .await
        })
    };
    let RpcResponse::IdeAction {
        action: Some(action),
    } = broker
        .handle(RpcRequest::PollIdeActions {
            session_id: session_id.clone(),
            wait_ms: 500,
        })
        .await
        .unwrap()
    else {
        panic!("context action was not queued");
    };
    assert!(matches!(action.action, IdeAction::GetEditorContext {}));
    broker
        .handle(RpcRequest::CompleteIdeAction {
            session_id: session_id.clone(),
            action_id: action.action_id,
            result: IdeActionResult::EditorContext {
                context: Some(IdeEditorContext {
                    active_file: file.clone(),
                    document_version: 7,
                    dirty: true,
                    selection: Some(crate::protocol::IdeSelection {
                        start: crate::protocol::IdePosition {
                            line: 0,
                            character: 0,
                        },
                        end: crate::protocol::IdePosition {
                            line: 0,
                            character: 8,
                        },
                        text: Some("PRIVATE_SELECTION_MARKER".into()),
                        selection_omitted: None,
                    }),
                }),
            },
        })
        .await
        .unwrap();
    let RpcResponse::IdeContext {
        context: Some(context),
    } = request.await.unwrap().unwrap()
    else {
        panic!("context response was not returned");
    };
    assert_eq!(
        context.selection.unwrap().text.as_deref(),
        Some("PRIVATE_SELECTION_MARKER")
    );

    let diagnostics_request = {
        let broker = Arc::clone(&broker);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            broker
                .handle(RpcRequest::GetIdeDiagnostics {
                    session_id,
                    file: None,
                    minimum_severity: Some(DiagnosticSeverity::Error),
                })
                .await
        })
    };
    let RpcResponse::IdeAction {
        action: Some(action),
    } = broker
        .handle(RpcRequest::PollIdeActions {
            session_id: session_id.clone(),
            wait_ms: 500,
        })
        .await
        .unwrap()
    else {
        panic!("diagnostics action was not queued");
    };
    broker
        .handle(RpcRequest::CompleteIdeAction {
            session_id,
            action_id: action.action_id,
            result: IdeActionResult::Diagnostics {
                items: vec![IdeDiagnostic {
                    path: file,
                    range: crate::protocol::IdeRange {
                        start: crate::protocol::IdePosition {
                            line: 0,
                            character: 0,
                        },
                        end: crate::protocol::IdePosition {
                            line: 0,
                            character: 2,
                        },
                    },
                    severity: DiagnosticSeverity::Error,
                    message: "PRIVATE_DIAGNOSTIC_MARKER".into(),
                    source: Some("test".into()),
                    code: Some("E1".into()),
                }],
                truncated: false,
            },
        })
        .await
        .unwrap();
    let RpcResponse::IdeDiagnostics(report) = diagnostics_request.await.unwrap().unwrap() else {
        panic!("diagnostics response was not returned");
    };
    assert_eq!(report.diagnostics[0].message, "PRIVATE_DIAGNOSTIC_MARKER");
    assert!(broker.events.lock().await.is_empty());
    assert!(!broker.event_log.exists());
}

#[tokio::test]
async fn ide_review_correlates_and_builds_add_update_delete_move_pairs() {
    let (_directory, broker, root) = test_broker();
    let update = root.join("update.rs");
    let delete = root.join("delete.rs");
    let move_from = root.join("move_from.rs");
    let move_to = root.join("move_to.rs");
    let add = root.join("add.rs");
    fs::write(&update, "old update\n").unwrap();
    fs::write(&delete, "old delete\n").unwrap();
    fs::write(&move_from, "old move\n").unwrap();
    let session_id = "b".repeat(IDE_SESSION_ID_HEX_LEN);
    register_test_ide(&broker, &root, &session_id).await;
    let targets = vec![
        EditTarget {
            kind: EditKind::Update,
            path: update.clone(),
            move_to: None,
        },
        EditTarget {
            kind: EditKind::Delete,
            path: delete.clone(),
            move_to: None,
        },
        EditTarget {
            kind: EditKind::Move,
            path: move_from.clone(),
            move_to: Some(move_to.clone()),
        },
        EditTarget {
            kind: EditKind::Add,
            path: add.clone(),
            move_to: None,
        },
    ];
    assert_eq!(
        broker
            .capture_ide_review(&session_id, "codex-session", "tool-call", &targets)
            .unwrap(),
        (true, false)
    );
    fs::write(&update, "new update\n").unwrap();
    fs::remove_file(&delete).unwrap();
    fs::rename(&move_from, &move_to).unwrap();
    fs::write(&add, "new add\n").unwrap();

    let request = {
        let broker = Arc::clone(&broker);
        tokio::spawn(async move {
            broker
                .handle(RpcRequest::OpenEditReview {
                    codex_session_id: "codex-session".into(),
                    tool_use_id: "tool-call".into(),
                })
                .await
        })
    };
    let RpcResponse::IdeAction {
        action: Some(action),
    } = broker
        .handle(RpcRequest::PollIdeActions {
            session_id: session_id.clone(),
            wait_ms: 500,
        })
        .await
        .unwrap()
    else {
        panic!("diff action was not queued");
    };
    let IdeAction::OpenDiff { pairs } = action.action else {
        panic!("queued action was not a diff");
    };
    assert_eq!(pairs.len(), 4);
    assert!(
        pairs
            .iter()
            .all(|pair| pair.left.is_file() && pair.right.is_file())
    );
    broker
        .handle(RpcRequest::CompleteIdeAction {
            session_id,
            action_id: action.action_id,
            result: IdeActionResult::DiffOpened {
                opened: pairs.len(),
                failed: 0,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        request.await.unwrap().unwrap(),
        RpcResponse::IdeReview {
            opened: 4,
            partial: false,
        }
    );
    assert_eq!(
        broker
            .handle(RpcRequest::OpenEditReview {
                codex_session_id: "other-session".into(),
                tool_use_id: "tool-call".into(),
            })
            .await
            .unwrap(),
        RpcResponse::IdeReview {
            opened: 0,
            partial: false,
        }
    );
    assert!(broker.events.lock().await.is_empty());
    assert!(!broker.event_log.exists());
}

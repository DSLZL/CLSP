use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::future::join_all;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    net::windows::named_pipe::NamedPipeServer,
    sync::{Mutex, Notify, RwLock, broadcast, oneshot},
};

use crate::{
    config::{Config, ConfigOverrides},
    installer::{
        ExecutableSource, ResolvedExecutable, ServerResolver, StatePaths, resolution_fingerprint,
    },
    ipc::{
        BrokerMetadata, apply_user_system_dacl, authenticate_server, create_pipe_server, pipe_name,
        publish_metadata, read_wire, verify_user_system_dacl, write_wire,
    },
    lsp::{LspClient, LspStartOptions, PositionEncoding, SyncResult, lsp_to_external},
    protocol::{
        BrokerEvent, BrokerSnapshot, ClientKey, ClspError, Diagnostic, DiagnosticSeverity,
        DiagnosticsReport, EditKind, EditTarget, ErrorCode, EventBody, EventEnvelope,
        IDE_ACTION_QUEUE_CAPACITY, IDE_DIAGNOSTICS_MAX_BYTES, IDE_SELECTION_MAX_BYTES,
        IDE_SESSION_ID_HEX_LEN, IDE_STDIO_MAX_BYTES, IdeAction, IdeActionEnvelope, IdeActionResult,
        IdeCandidate, IdeDiagnostic, IdeDiagnosticsReport, IdeDiffPair, IdeEditorContext,
        IdePrepareOutcome, PROTOCOL_VERSION, QueryRequest, ResponseEnvelope, RpcRequest,
        RpcResponse, ServerSnapshot, ServerState, TextRange, WireMessage,
    },
    registry::{InstallRecipe, Registry, ServerDefinition},
    workspace::{Detection, Workspace},
};

const EVENT_RING_CAPACITY: usize = 256;
const EVENT_LOG_MAX_BYTES: u64 = 1024 * 1024;
const IDE_SESSION_TTL: Duration = Duration::from_secs(6);
const IDE_DIAGNOSTIC_BASELINE_CAPACITY: usize = 64;
const IDE_REVIEW_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const IDE_REVIEW_SCHEMA: u8 = 1;
const FSHARP_SERVER_ID: &str = "fsharp";
const JDTLS_SERVER_ID: &str = "jdtls";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IdeReviewManifest {
    schema: u8,
    ide_session_id: String,
    partial: bool,
    targets: Vec<IdeReviewTarget>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IdeReviewTarget {
    kind: EditKind,
    path: PathBuf,
    move_to: Option<PathBuf>,
    before_file: String,
    after_empty_file: String,
    before_sha256: String,
    before_exists: bool,
}

struct ManagedServer {
    key: ClientKey,
    definition: ServerDefinition,
    state: ServerState,
    executable: Option<PathBuf>,
    client: Option<Arc<LspClient>>,
    detail: Option<String>,
    install_progress: Option<f32>,
    failures: u32,
    retry_after: Option<Instant>,
    last_used: Instant,
}

struct ServerHandle {
    ensure: Mutex<()>,
    inner: Mutex<ManagedServer>,
}

#[derive(Default)]
struct LeaseBook {
    leases: HashMap<String, Instant>,
}

impl LeaseBook {
    fn renew(&mut self, session_id: String, ttl: Duration, now: Instant) {
        self.leases.insert(session_id, now + ttl);
    }

    fn release(&mut self, session_id: &str) -> bool {
        self.leases.remove(session_id).is_some()
    }

    fn sweep(&mut self, now: Instant) -> Vec<String> {
        let expired: Vec<_> = self
            .leases
            .iter()
            .filter(|(_, expires)| **expires <= now)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        self.leases.retain(|_, expires| *expires > now);
        expired
    }
}

struct IdeSession {
    adapter_version: String,
    workspace_root: PathBuf,
    last_seen: Instant,
    queue: VecDeque<IdeActionEnvelope>,
    pending: HashMap<u64, oneshot::Sender<IdeActionResult>>,
    diagnostic_baselines: BTreeMap<String, IdeDiagnosticBaseline>,
    notify: Arc<Notify>,
}

struct IdeDiagnosticBaseline {
    paths: BTreeSet<PathBuf>,
    diagnostics: Vec<IdeDiagnostic>,
    complete: bool,
}

#[derive(Default)]
struct IdeRegistry {
    sessions: HashMap<String, IdeSession>,
    next_action_id: u64,
}

impl IdeRegistry {
    fn live_session(&mut self, session_id: &str, now: Instant) -> Option<&mut IdeSession> {
        self.sessions
            .get_mut(session_id)
            .filter(|session| now.duration_since(session.last_seen) <= IDE_SESSION_TTL)
    }

    fn sweep(&mut self, now: Instant) {
        self.sessions
            .retain(|_, session| now.duration_since(session.last_seen) <= IDE_SESSION_TTL);
    }

    fn enqueue(
        &mut self,
        session_id: &str,
        action: IdeAction,
        now: Instant,
    ) -> Result<(u64, oneshot::Receiver<IdeActionResult>, Arc<Notify>), ClspError> {
        let next = self.next_action_id.max(1);
        self.next_action_id = next.saturating_add(1).max(1);
        let session = self
            .live_session(session_id, now)
            .ok_or_else(ide_unavailable)?;
        if session.pending.len() >= IDE_ACTION_QUEUE_CAPACITY {
            return Err(
                ClspError::new(ErrorCode::IdeUnavailable, "IDE action queue is full").retryable(),
            );
        }
        let (sender, receiver) = oneshot::channel();
        session.pending.insert(next, sender);
        session.queue.push_back(IdeActionEnvelope {
            action_id: next,
            action,
        });
        Ok((next, receiver, Arc::clone(&session.notify)))
    }

    fn cancel(&mut self, session_id: &str, action_id: u64) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.pending.remove(&action_id);
            session.queue.retain(|action| action.action_id != action_id);
        }
    }
}

pub struct Broker {
    config: Config,
    workspace: Workspace,
    registry: Registry,
    resolver: Arc<ServerResolver>,
    servers: RwLock<BTreeMap<ClientKey, Arc<ServerHandle>>>,
    leases: Mutex<LeaseBook>,
    ide: Mutex<IdeRegistry>,
    events: Mutex<VecDeque<BrokerEvent>>,
    watcher_changes: Mutex<BTreeSet<PathBuf>>,
    watcher_baselines: Mutex<BTreeMap<(ClientKey, PathBuf), SyncResult>>,
    resolutions: Mutex<BTreeMap<ClientKey, ResolvedExecutable>>,
    event_tx: broadcast::Sender<BrokerEvent>,
    sequence: AtomicU64,
    connections: AtomicUsize,
    active_work: Arc<AtomicUsize>,
    last_activity_ms: AtomicU64,
    hook_last_seen_ms: AtomicU64,
    shutting_down: AtomicBool,
    shutdown: Notify,
    config_digest: String,
    event_log: PathBuf,
}

impl Broker {
    pub fn new(
        config: Config,
        workspace: Workspace,
        registry: Registry,
        paths: StatePaths,
    ) -> Result<Arc<Self>, ClspError> {
        let config_digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&config).map_err(broker_error)?,
        ));
        let event_log = paths.logs.join("events.jsonl");
        cleanup_ide_reviews(&paths.workspace_state.join("ide-review"));
        let resolver = Arc::new(ServerResolver::new(config.clone(), paths));
        let (event_tx, _) = broadcast::channel(EVENT_RING_CAPACITY);
        Ok(Arc::new(Self {
            config,
            workspace,
            registry,
            resolver,
            servers: RwLock::new(BTreeMap::new()),
            leases: Mutex::new(LeaseBook::default()),
            ide: Mutex::new(IdeRegistry::default()),
            events: Mutex::new(VecDeque::new()),
            watcher_changes: Mutex::new(BTreeSet::new()),
            watcher_baselines: Mutex::new(BTreeMap::new()),
            resolutions: Mutex::new(BTreeMap::new()),
            event_tx,
            sequence: AtomicU64::new(0),
            connections: AtomicUsize::new(0),
            active_work: Arc::new(AtomicUsize::new(0)),
            last_activity_ms: AtomicU64::new(now_ms()),
            hook_last_seen_ms: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            shutdown: Notify::new(),
            config_digest,
            event_log,
        }))
    }

    pub fn paths(&self) -> &StatePaths {
        self.resolver.paths()
    }

    pub async fn handle(self: &Arc<Self>, request: RpcRequest) -> Result<RpcResponse, ClspError> {
        self.touch();
        match request {
            RpcRequest::AcquireLease { session_id } => {
                self.renew_lease(session_id.clone()).await?;
                let broker = Arc::clone(self);
                tokio::spawn(async move {
                    let _ = broker.discover_and_register(broker.config.prewarm).await;
                });
                Ok(RpcResponse::Ack)
            }
            RpcRequest::RenewLease { session_id } => {
                self.hook_last_seen_ms.store(now_ms(), Ordering::Relaxed);
                self.renew_lease(session_id).await?;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::ReleaseLease { session_id } => {
                self.release_lease(&session_id).await;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::Discover => {
                let broker = Arc::clone(self);
                tokio::spawn(async move {
                    let _ = broker.discover_and_register(broker.config.prewarm).await;
                });
                Ok(RpcResponse::Ack)
            }
            RpcRequest::EnsureFile { path } => {
                self.ensure_file(&path).await?;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::Query(request) => Ok(RpcResponse::Query(self.query(request).await?)),
            RpcRequest::Diagnostics {
                paths,
                minimum_severity,
                wait_ms,
            } => Ok(RpcResponse::Diagnostics(
                self.diagnostics(&paths, minimum_severity, wait_ms).await?,
            )),
            RpcRequest::Snapshot => Ok(RpcResponse::Snapshot(self.snapshot().await)),
            RpcRequest::Subscribe { after_seq } => Ok(RpcResponse::Events {
                events: self.events_after(after_seq).await,
            }),
            RpcRequest::RetryServer { key } => {
                self.clear_retry(&key).await?;
                self.ensure_key(&key).await?;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::StartServer { key } => {
                self.ensure_key(&key).await?;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::StopServer { key } => {
                self.stop_key(&key).await?;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::SyncFiles { paths } => {
                let paths = self.merge_watcher_changes(paths).await;
                self.sync_files(&paths, false).await
            }
            RpcRequest::SyncIdeDiagnostics {
                session_id,
                codex_session_id,
                tool_use_id,
                paths,
            } => {
                self.sync_ide_diagnostics(&session_id, &codex_session_id, &tool_use_id, &paths)
                    .await
            }
            RpcRequest::RegisterIde {
                session_id,
                adapter_version,
                workspace_root,
            } => {
                self.register_ide(session_id, adapter_version, workspace_root)
                    .await?;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::UnregisterIde { session_id } => {
                validate_ide_session_id(&session_id)?;
                self.ide.lock().await.sessions.remove(&session_id);
                Ok(RpcResponse::Ack)
            }
            RpcRequest::PollIdeActions {
                session_id,
                wait_ms,
            } => self.poll_ide_actions(&session_id, wait_ms).await,
            RpcRequest::CompleteIdeAction {
                session_id,
                action_id,
                result,
            } => {
                self.complete_ide_action(&session_id, action_id, result)
                    .await?;
                Ok(RpcResponse::Ack)
            }
            RpcRequest::ListIdeCandidates { cwd } => self.list_ide_candidates(&cwd).await,
            RpcRequest::GetIdeContext { session_id } => self.get_ide_context(&session_id).await,
            RpcRequest::GetIdeDiagnostics {
                session_id,
                file,
                minimum_severity,
            } => {
                self.get_ide_diagnostics(&session_id, file.as_deref(), minimum_severity)
                    .await
            }
            RpcRequest::PrepareEdit {
                session_id,
                codex_session_id,
                tool_use_id,
                targets,
            } => {
                self.prepare_ide_edit(&session_id, &codex_session_id, &tool_use_id, &targets)
                    .await
            }
            RpcRequest::OpenEditReview {
                codex_session_id,
                tool_use_id,
            } => self.open_ide_review(&codex_session_id, &tool_use_id).await,
        }
    }

    async fn renew_lease(&self, session_id: String) -> Result<(), ClspError> {
        validate_session_id(&session_id)?;
        self.leases.lock().await.renew(
            session_id.clone(),
            Duration::from_secs(self.config.lifecycle.session_lease_seconds),
            Instant::now(),
        );
        self.publish(EventBody::LeaseChanged {
            session_id,
            active: true,
        })
        .await;
        Ok(())
    }

    async fn release_lease(&self, session_id: &str) {
        if self.leases.lock().await.release(session_id) {
            self.publish(EventBody::LeaseChanged {
                session_id: session_id.into(),
                active: false,
            })
            .await;
        }
    }

    async fn register_ide(
        &self,
        session_id: String,
        adapter_version: String,
        workspace_root: PathBuf,
    ) -> Result<(), ClspError> {
        validate_ide_session_id(&session_id)?;
        if adapter_version != env!("CARGO_PKG_VERSION") {
            return Err(ClspError::new(
                ErrorCode::ProtocolMismatch,
                "CLSP IDE adapter version does not match clsp.exe",
            ));
        }
        let registered = Workspace::open(&workspace_root)?;
        if registered.root() != self.workspace.root() {
            return Err(ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "IDE registration root does not match this Broker",
            )
            .for_path(workspace_root));
        }
        let now = Instant::now();
        let mut ide = self.ide.lock().await;
        match ide.sessions.get_mut(&session_id) {
            Some(session) => {
                session.adapter_version = adapter_version;
                session.workspace_root = registered.root().to_path_buf();
                session.last_seen = now;
            }
            None => {
                ide.sessions.insert(
                    session_id,
                    IdeSession {
                        adapter_version,
                        workspace_root: registered.root().to_path_buf(),
                        last_seen: now,
                        queue: VecDeque::new(),
                        pending: HashMap::new(),
                        diagnostic_baselines: BTreeMap::new(),
                        notify: Arc::new(Notify::new()),
                    },
                );
            }
        }
        Ok(())
    }

    async fn poll_ide_actions(
        &self,
        session_id: &str,
        wait_ms: u64,
    ) -> Result<RpcResponse, ClspError> {
        validate_ide_session_id(session_id)?;
        if wait_ms > 2_000 {
            return Err(ClspError::new(
                ErrorCode::InvalidRequest,
                "IDE poll wait exceeds two seconds",
            ));
        }
        let notify = {
            let mut ide = self.ide.lock().await;
            let session = ide
                .live_session(session_id, Instant::now())
                .ok_or_else(ide_unavailable)?;
            session.last_seen = Instant::now();
            if let Some(action) = session.queue.pop_front() {
                return Ok(RpcResponse::IdeAction {
                    action: Some(action),
                });
            }
            Arc::clone(&session.notify)
        };
        let _ = tokio::time::timeout(Duration::from_millis(wait_ms), notify.notified()).await;
        let mut ide = self.ide.lock().await;
        let session = ide
            .live_session(session_id, Instant::now())
            .ok_or_else(ide_unavailable)?;
        session.last_seen = Instant::now();
        Ok(RpcResponse::IdeAction {
            action: session.queue.pop_front(),
        })
    }

    async fn complete_ide_action(
        &self,
        session_id: &str,
        action_id: u64,
        result: IdeActionResult,
    ) -> Result<(), ClspError> {
        validate_ide_session_id(session_id)?;
        let mut ide = self.ide.lock().await;
        let session = ide
            .live_session(session_id, Instant::now())
            .ok_or_else(ide_unavailable)?;
        session.last_seen = Instant::now();
        let sender = session.pending.remove(&action_id).ok_or_else(|| {
            ClspError::new(
                ErrorCode::InvalidRequest,
                "IDE action is unknown or expired",
            )
        })?;
        sender.send(result).map_err(|_| ide_unavailable())
    }

    async fn request_ide_action(
        &self,
        session_id: &str,
        action: IdeAction,
        limit: Duration,
    ) -> Result<IdeActionResult, ClspError> {
        validate_ide_session_id(session_id)?;
        let (action_id, receiver, notify) =
            self.ide
                .lock()
                .await
                .enqueue(session_id, action, Instant::now())?;
        notify.notify_one();
        match tokio::time::timeout(limit, receiver).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(ide_unavailable()),
            Err(_) => {
                self.ide.lock().await.cancel(session_id, action_id);
                Err(ClspError::new(ErrorCode::IdeUnavailable, "IDE action timed out").retryable())
            }
        }
    }

    async fn list_ide_candidates(&self, cwd: &Path) -> Result<RpcResponse, ClspError> {
        if !self.workspace.contains_existing(cwd) {
            return Err(ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "candidate cwd is outside this Broker workspace",
            )
            .for_path(cwd));
        }
        let now = Instant::now();
        let mut ide = self.ide.lock().await;
        ide.sweep(now);
        let candidates = ide
            .sessions
            .iter()
            .map(|(session_id, session)| IdeCandidate {
                session_id: session_id.clone(),
                workspace_root: session.workspace_root.clone(),
            })
            .collect();
        Ok(RpcResponse::IdeCandidates { candidates })
    }

    async fn has_live_ide_session(&self) -> bool {
        let mut ide = self.ide.lock().await;
        ide.sweep(Instant::now());
        !ide.sessions.is_empty()
    }

    async fn get_ide_context(&self, session_id: &str) -> Result<RpcResponse, ClspError> {
        let result = self
            .request_ide_action(
                session_id,
                IdeAction::GetEditorContext {},
                Duration::from_millis(500),
            )
            .await?;
        let IdeActionResult::EditorContext { context } = result else {
            return Err(ide_result_error(result));
        };
        let context = context
            .map(|context| self.sanitize_editor_context(context))
            .transpose()?
            .flatten();
        Ok(RpcResponse::IdeContext { context })
    }

    fn sanitize_editor_context(
        &self,
        mut context: IdeEditorContext,
    ) -> Result<Option<IdeEditorContext>, ClspError> {
        let path = self
            .workspace
            .resolve_file(&context.active_file, self.config.limits.max_file_bytes)?;
        let relative = path.strip_prefix(self.workspace.root()).map_err(|_| {
            ClspError::new(
                ErrorCode::PathOutsideWorkspace,
                "IDE active file is outside the workspace",
            )
        })?;
        if self.config.ide.is_denied(relative)? {
            return Ok(None);
        }
        if let Some(selection) = context.selection.as_mut() {
            if selection.end < selection.start {
                return Err(ClspError::new(
                    ErrorCode::InvalidRequest,
                    "IDE selection range is invalid",
                ));
            }
            if selection
                .selection_omitted
                .as_deref()
                .is_some_and(|reason| reason != "too_large")
                || (selection.text.is_some() && selection.selection_omitted.is_some())
            {
                return Err(ClspError::new(
                    ErrorCode::InvalidRequest,
                    "IDE selection omission reason is invalid",
                ));
            }
            if selection
                .text
                .as_ref()
                .is_some_and(|text| text.len() > IDE_SELECTION_MAX_BYTES)
            {
                selection.text = None;
                selection.selection_omitted = Some("too_large".into());
            }
        }
        context.active_file = relative.to_path_buf();
        Ok(Some(context))
    }

    async fn get_ide_diagnostics(
        &self,
        session_id: &str,
        file: Option<&Path>,
        minimum_severity: Option<DiagnosticSeverity>,
    ) -> Result<RpcResponse, ClspError> {
        let file = file
            .map(|path| {
                self.workspace
                    .resolve_file(path, self.config.limits.max_file_bytes)
            })
            .transpose()?;
        if let Some(path) = file.as_ref() {
            let relative = path.strip_prefix(self.workspace.root()).map_err(|_| {
                ClspError::new(
                    ErrorCode::PathOutsideWorkspace,
                    "diagnostic path is outside workspace",
                )
            })?;
            if self.config.ide.is_denied(relative)? {
                return Err(ClspError::new(
                    ErrorCode::IdeUnavailable,
                    "diagnostic path is denied by IDE policy",
                )
                .retryable()
                .for_path(relative));
            }
        }
        let minimum = minimum_severity.unwrap_or(self.config.diagnostics.minimum_severity);
        let result = self
            .request_ide_action(
                session_id,
                IdeAction::GetDiagnostics {
                    file,
                    minimum_severity: minimum,
                },
                Duration::from_secs(2),
            )
            .await?;
        let IdeActionResult::Diagnostics { items, truncated } = result else {
            return Err(ide_result_error(result));
        };
        let report = self.sanitize_ide_diagnostics(items, minimum, truncated)?;
        Ok(RpcResponse::IdeDiagnostics(report))
    }

    fn sanitize_ide_diagnostics(
        &self,
        diagnostics: Vec<IdeDiagnostic>,
        minimum: DiagnosticSeverity,
        mut truncated: bool,
    ) -> Result<IdeDiagnosticsReport, ClspError> {
        let mut per_file = BTreeMap::<PathBuf, usize>::new();
        let mut items = Vec::new();
        for mut diagnostic in diagnostics {
            if severity_rank(diagnostic.severity) > severity_rank(minimum) {
                continue;
            }
            let Ok(path) = self
                .workspace
                .resolve_file(&diagnostic.path, self.config.limits.max_file_bytes)
            else {
                truncated = true;
                continue;
            };
            let relative = path.strip_prefix(self.workspace.root()).map_err(|_| {
                ClspError::new(
                    ErrorCode::PathOutsideWorkspace,
                    "IDE diagnostic escaped workspace",
                )
            })?;
            if self.config.ide.is_denied(relative)? {
                truncated = true;
                continue;
            }
            if !per_file.contains_key(relative)
                && per_file.len() >= self.config.diagnostics.max_files
            {
                truncated = true;
                continue;
            }
            let count = per_file.entry(relative.to_path_buf()).or_default();
            if *count >= self.config.diagnostics.max_per_file {
                truncated = true;
                continue;
            }
            *count += 1;
            diagnostic.path = relative.to_path_buf();
            truncate_utf8(&mut diagnostic.message, 4 * 1024);
            if let Some(source) = diagnostic.source.as_mut() {
                truncate_utf8(source, 256);
            }
            if let Some(code) = diagnostic.code.as_mut() {
                truncate_utf8(code, 256);
            }
            if diagnostic.range.end < diagnostic.range.start {
                truncated = true;
                continue;
            }
            items.push(diagnostic);
        }
        items.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.range.cmp(&right.range))
                .then_with(|| severity_rank(left.severity).cmp(&severity_rank(right.severity)))
                .then_with(|| left.message.cmp(&right.message))
        });
        let mut report = IdeDiagnosticsReport {
            source: "vscode_problems".into(),
            fresh: true,
            position_encoding: "utf-16".into(),
            diagnostics: items,
            truncated,
        };
        while serde_json::to_vec(&report).map_err(broker_error)?.len() > IDE_DIAGNOSTICS_MAX_BYTES {
            if report.diagnostics.pop().is_none() {
                break;
            }
            report.truncated = true;
        }
        Ok(report)
    }

    async fn ide_diagnostics_for_paths(
        &self,
        session_id: &str,
        paths: &[PathBuf],
        limit: Duration,
    ) -> Result<IdeDiagnosticsReport, ClspError> {
        let results = join_all(paths.iter().cloned().map(|file| async move {
            let result = self
                .request_ide_action(
                    session_id,
                    IdeAction::GetDiagnostics {
                        file: Some(file.clone()),
                        minimum_severity: DiagnosticSeverity::Error,
                    },
                    limit,
                )
                .await;
            (file, result)
        }))
        .await;
        let mut items = Vec::new();
        let mut truncated = false;
        for (expected, result) in results {
            let result = result?;
            let IdeActionResult::Diagnostics {
                items: diagnostics,
                truncated: action_truncated,
            } = result
            else {
                return Err(ide_result_error(result));
            };
            truncated |= action_truncated;
            for diagnostic in diagnostics {
                match self
                    .workspace
                    .resolve_file(&diagnostic.path, self.config.limits.max_file_bytes)
                {
                    Ok(path) if path == expected => items.push(diagnostic),
                    _ => truncated = true,
                }
            }
        }
        self.sanitize_ide_diagnostics(items, DiagnosticSeverity::Error, truncated)
    }

    async fn capture_ide_diagnostic_baseline(
        &self,
        session_id: &str,
        codex_session_id: &str,
        tool_use_id: &str,
        paths: &[PathBuf],
    ) {
        let selected: Vec<_> = paths
            .iter()
            .take(self.config.diagnostics.max_files)
            .cloned()
            .collect();
        let mut complete = selected.len() == paths.len();
        let mut relative_paths = BTreeSet::new();
        for path in &selected {
            match path.strip_prefix(self.workspace.root()) {
                Ok(relative) => {
                    relative_paths.insert(relative.to_path_buf());
                }
                Err(_) => complete = false,
            }
        }
        let diagnostics = match self
            .ide_diagnostics_for_paths(session_id, &selected, Duration::from_millis(500))
            .await
        {
            Ok(report) => {
                complete &= !report.truncated;
                report.diagnostics
            }
            Err(_) => {
                complete = false;
                Vec::new()
            }
        };
        let baseline = IdeDiagnosticBaseline {
            paths: relative_paths,
            diagnostics,
            complete,
        };
        let key = correlation_hash(codex_session_id, tool_use_id);
        let mut ide = self.ide.lock().await;
        let Some(session) = ide.live_session(session_id, Instant::now()) else {
            return;
        };
        while session.diagnostic_baselines.len() >= IDE_DIAGNOSTIC_BASELINE_CAPACITY {
            session.diagnostic_baselines.pop_first();
        }
        session.diagnostic_baselines.insert(key, baseline);
    }

    async fn sync_ide_diagnostics(
        &self,
        session_id: &str,
        codex_session_id: &str,
        tool_use_id: &str,
        inputs: &[PathBuf],
    ) -> Result<RpcResponse, ClspError> {
        validate_ide_session_id(session_id)?;
        validate_correlation_id("Codex session", codex_session_id)?;
        validate_correlation_id("tool use", tool_use_id)?;
        if inputs.len() > self.config.diagnostics.max_files {
            return Err(ClspError::new(
                ErrorCode::InvalidRequest,
                "IDE sync path count is outside configured bounds",
            ));
        }
        let mut paths = Vec::new();
        let mut relative_paths = BTreeSet::new();
        for input in inputs {
            let path = self
                .workspace
                .resolve_file(input, self.config.limits.max_file_bytes)?;
            let relative = path.strip_prefix(self.workspace.root()).map_err(|_| {
                ClspError::new(
                    ErrorCode::PathOutsideWorkspace,
                    "IDE sync path escaped workspace",
                )
            })?;
            relative_paths.insert(relative.to_path_buf());
            paths.push(path);
        }
        let baseline = {
            let mut ide = self.ide.lock().await;
            let session = ide
                .live_session(session_id, Instant::now())
                .ok_or_else(ide_unavailable)?;
            session
                .diagnostic_baselines
                .remove(&correlation_hash(codex_session_id, tool_use_id))
        };
        let Some(baseline) = baseline else {
            return Ok(RpcResponse::Sync {
                paths,
                new_errors: Vec::new(),
                fresh: false,
                baseline_available: false,
            });
        };
        if !baseline.complete || !relative_paths.is_subset(&baseline.paths) {
            return Ok(RpcResponse::Sync {
                paths,
                new_errors: Vec::new(),
                fresh: false,
                baseline_available: false,
            });
        }
        tokio::time::sleep(Duration::from_millis(
            self.config.diagnostics.wait_ms.min(5_000),
        ))
        .await;
        let report = self
            .ide_diagnostics_for_paths(session_id, &paths, Duration::from_secs(1))
            .await?;
        if report.truncated {
            return Ok(RpcResponse::Sync {
                paths,
                new_errors: Vec::new(),
                fresh: true,
                baseline_available: false,
            });
        }
        let new_diagnostics: Vec<_> = report
            .diagnostics
            .into_iter()
            .filter(|diagnostic| {
                diagnostic.severity == DiagnosticSeverity::Error
                    && !baseline.diagnostics.contains(diagnostic)
            })
            .collect();
        let diagnostic_paths: BTreeSet<_> = new_diagnostics
            .iter()
            .map(|diagnostic| diagnostic.path.clone())
            .collect();
        let mut texts = BTreeMap::new();
        for relative in diagnostic_paths {
            let path = self
                .workspace
                .resolve_file(&relative, self.config.limits.max_file_bytes)?;
            texts.insert(relative, fs::read_to_string(path).map_err(broker_error)?);
        }
        let mut new_errors = Vec::with_capacity(new_diagnostics.len());
        for diagnostic in new_diagnostics {
            let text = texts.get(&diagnostic.path).ok_or_else(|| {
                ClspError::new(
                    ErrorCode::IdeUnavailable,
                    "IDE diagnostic text is unavailable",
                )
            })?;
            new_errors.push(ide_diagnostic_to_protocol(diagnostic, text)?);
        }
        Ok(RpcResponse::Sync {
            paths,
            new_errors,
            fresh: true,
            baseline_available: true,
        })
    }

    async fn prepare_ide_edit(
        &self,
        session_id: &str,
        codex_session_id: &str,
        tool_use_id: &str,
        targets: &[EditTarget],
    ) -> Result<RpcResponse, ClspError> {
        validate_correlation_id("Codex session", codex_session_id)?;
        validate_correlation_id("tool use", tool_use_id)?;
        if targets.is_empty() || targets.len() > 64 {
            return Err(ClspError::new(
                ErrorCode::InvalidRequest,
                "IDE edit target count is outside supported bounds",
            ));
        }
        let mut paths = BTreeSet::new();
        for target in targets {
            if (target.kind == EditKind::Move) != target.move_to.is_some() {
                return Err(ClspError::new(
                    ErrorCode::InvalidRequest,
                    "only move targets may specify a destination",
                ));
            }
            let source = match target.kind {
                EditKind::Add => self.workspace.resolve_candidate(&target.path)?,
                EditKind::Update | EditKind::Delete | EditKind::Move => self
                    .workspace
                    .resolve_file(&target.path, self.config.limits.max_file_bytes)?,
            };
            paths.insert(source.clone());
            if let Some(destination) = target.move_to.as_ref() {
                let destination = self.workspace.resolve_candidate(destination)?;
                if destination == source {
                    return Err(ClspError::new(
                        ErrorCode::InvalidRequest,
                        "IDE move source and destination must differ",
                    ));
                }
                paths.insert(destination);
            }
        }
        if paths.len() > 64 {
            return Err(ClspError::new(
                ErrorCode::InvalidRequest,
                "IDE edit names more than 64 distinct paths; split the edit",
            ));
        }
        let paths: Vec<_> = paths.into_iter().collect();
        let result = self
            .request_ide_action(
                session_id,
                IdeAction::PrepareEdit {
                    targets: paths.clone(),
                },
                Duration::from_secs(25),
            )
            .await?;
        match result {
            IdeActionResult::Prepared {
                outcome: IdePrepareOutcome::Ready,
                ..
            } => {
                self.capture_ide_diagnostic_baseline(
                    session_id,
                    codex_session_id,
                    tool_use_id,
                    &paths,
                )
                .await;
                let (review_available, partial) = self
                    .capture_ide_review(session_id, codex_session_id, tool_use_id, targets)
                    .unwrap_or((false, true));
                Ok(RpcResponse::IdePrepared {
                    review_available,
                    partial,
                })
            }
            IdeActionResult::Prepared { message, .. } => Err(ClspError::new(
                ErrorCode::IdeUnavailable,
                message.unwrap_or_else(|| "IDE edit preparation was cancelled".into()),
            )),
            result => Err(ide_result_error(result)),
        }
    }

    fn capture_ide_review(
        &self,
        ide_session_id: &str,
        codex_session_id: &str,
        tool_use_id: &str,
        targets: &[EditTarget],
    ) -> Result<(bool, bool), ClspError> {
        let root = self.paths().workspace_state.join("ide-review");
        cleanup_ide_reviews(&root);
        fs::create_dir_all(&root).map_err(broker_error)?;
        apply_user_system_dacl(&root, true)?;
        let directory = root.join(correlation_hash(codex_session_id, tool_use_id));
        if directory.exists() {
            let metadata = fs::symlink_metadata(&directory).map_err(broker_error)?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || verify_user_system_dacl(&directory).is_err()
            {
                return Err(ClspError::new(
                    ErrorCode::IdeUnavailable,
                    "existing IDE review directory is not protected",
                ));
            }
            fs::remove_dir_all(&directory).map_err(broker_error)?;
        }
        fs::create_dir(&directory).map_err(broker_error)?;
        apply_user_system_dacl(&directory, true)?;

        let review_limit = self.config.diagnostics.max_files.min(5);
        let total_limit = self
            .config
            .limits
            .max_file_bytes
            .saturating_mul(review_limit as u64);
        let mut total_bytes = 0u64;
        let mut partial = targets.len() > review_limit;
        let mut captured = Vec::new();
        for (index, target) in targets.iter().take(review_limit).enumerate() {
            let source = match target.kind {
                EditKind::Add => self.workspace.resolve_candidate(&target.path),
                EditKind::Update | EditKind::Delete | EditKind::Move => self
                    .workspace
                    .resolve_file(&target.path, self.config.limits.max_file_bytes),
            };
            let Ok(source) = source else {
                partial = true;
                continue;
            };
            if target.kind == EditKind::Add && source.exists() {
                partial = true;
                continue;
            }
            let relative = match self.workspace.relative_candidate(&source) {
                Ok(relative) => relative,
                Err(_) => {
                    partial = true;
                    continue;
                }
            };
            let move_to = match target.move_to.as_ref() {
                Some(destination) => match self
                    .workspace
                    .resolve_candidate(destination)
                    .and_then(|path| self.workspace.relative_candidate(&path))
                {
                    Ok(relative) => Some(relative),
                    Err(_) => {
                        partial = true;
                        continue;
                    }
                },
                None => None,
            };
            let before_exists = target.kind != EditKind::Add;
            let before = if before_exists {
                match read_review_file(&source, self.config.limits.max_file_bytes) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        partial = true;
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            if total_bytes.saturating_add(before.len() as u64) > total_limit {
                partial = true;
                continue;
            }
            total_bytes += before.len() as u64;
            let before_file = review_file_name(index, "before", &relative);
            let after_empty_file = review_file_name(index, "after", &relative);
            write_review_file(&directory.join(&before_file), &before)?;
            write_review_file(&directory.join(&after_empty_file), &[])?;
            captured.push(IdeReviewTarget {
                kind: target.kind,
                path: relative,
                move_to,
                before_file,
                after_empty_file,
                before_sha256: hash_bytes(&before),
                before_exists,
            });
        }

        let manifest = IdeReviewManifest {
            schema: IDE_REVIEW_SCHEMA,
            ide_session_id: ide_session_id.to_owned(),
            partial,
            targets: captured,
        };
        let bytes = serde_json::to_vec(&manifest).map_err(broker_error)?;
        if bytes.len() > IDE_STDIO_MAX_BYTES {
            let _ = fs::remove_dir_all(&directory);
            return Ok((false, true));
        }
        write_review_file(&directory.join("manifest.json"), &bytes)?;
        Ok((!manifest.targets.is_empty(), manifest.partial))
    }

    async fn open_ide_review(
        &self,
        codex_session_id: &str,
        tool_use_id: &str,
    ) -> Result<RpcResponse, ClspError> {
        validate_correlation_id("Codex session", codex_session_id)?;
        validate_correlation_id("tool use", tool_use_id)?;
        let directory = self
            .paths()
            .workspace_state
            .join("ide-review")
            .join(correlation_hash(codex_session_id, tool_use_id));
        let manifest_path = directory.join("manifest.json");
        if !manifest_path.exists() {
            return Ok(RpcResponse::IdeReview {
                opened: 0,
                partial: false,
            });
        }
        verify_user_system_dacl(&directory)?;
        verify_user_system_dacl(&manifest_path)?;
        let bytes = fs::read(&manifest_path).map_err(broker_error)?;
        if bytes.len() > IDE_STDIO_MAX_BYTES {
            return Err(ClspError::new(
                ErrorCode::IdeUnavailable,
                "IDE review manifest exceeds its limit",
            ));
        }
        let manifest: IdeReviewManifest = serde_json::from_slice(&bytes).map_err(broker_error)?;
        if manifest.schema != IDE_REVIEW_SCHEMA
            || !valid_ide_session_id(&manifest.ide_session_id)
            || manifest.targets.len() > self.config.diagnostics.max_files.min(5)
        {
            return Err(ClspError::new(
                ErrorCode::IdeUnavailable,
                "IDE review manifest is invalid",
            ));
        }

        let mut partial = manifest.partial;
        let mut pairs = Vec::new();
        for target in &manifest.targets {
            let Some(before) = review_child(&directory, &target.before_file) else {
                partial = true;
                continue;
            };
            let Some(after_empty) = review_child(&directory, &target.after_empty_file) else {
                partial = true;
                continue;
            };
            if verify_user_system_dacl(&before).is_err()
                || verify_user_system_dacl(&after_empty).is_err()
            {
                partial = true;
                continue;
            }
            let Ok(before_bytes) = read_review_file(&before, self.config.limits.max_file_bytes)
            else {
                partial = true;
                continue;
            };
            if hash_bytes(&before_bytes) != target.before_sha256 {
                partial = true;
                continue;
            }
            let current_relative = if target.kind == EditKind::Move {
                target.move_to.as_ref().unwrap_or(&target.path)
            } else {
                &target.path
            };
            let Ok(candidate) = self.workspace.resolve_candidate(current_relative) else {
                partial = true;
                continue;
            };
            let exists = candidate.exists();
            if matches!(target.kind, EditKind::Add | EditKind::Move) && !exists {
                continue;
            }
            let (right, after_hash) = if exists {
                let Ok(current) = self
                    .workspace
                    .resolve_file(&candidate, self.config.limits.max_file_bytes)
                else {
                    partial = true;
                    continue;
                };
                let Ok(contents) = read_review_file(&current, self.config.limits.max_file_bytes)
                else {
                    partial = true;
                    continue;
                };
                (current, hash_bytes(&contents))
            } else {
                (after_empty, hash_bytes(&[]))
            };
            if target.kind != EditKind::Move
                && target.before_exists == exists
                && target.before_sha256 == after_hash
            {
                continue;
            }
            let mut title = format!(
                "Before Codex <-> After Codex: {}",
                current_relative.display()
            );
            truncate_utf8(&mut title, 256);
            pairs.push(IdeDiffPair {
                left: before,
                right,
                title,
            });
        }
        if pairs.is_empty() {
            return Ok(RpcResponse::IdeReview { opened: 0, partial });
        }
        let expected = pairs.len();
        let result = match self
            .request_ide_action(
                &manifest.ide_session_id,
                IdeAction::OpenDiff { pairs },
                Duration::from_secs(3),
            )
            .await
        {
            Ok(result) => result,
            Err(_) => {
                return Ok(RpcResponse::IdeReview {
                    opened: 0,
                    partial: true,
                });
            }
        };
        let IdeActionResult::DiffOpened { opened, failed } = result else {
            return Err(ide_result_error(result));
        };
        Ok(RpcResponse::IdeReview {
            opened,
            partial: partial || failed > 0 || opened < expected,
        })
    }

    async fn discover_and_register(self: &Arc<Self>, prewarm: bool) -> Result<(), ClspError> {
        let _work = WorkGuard::new(Arc::clone(&self.active_work));
        let prewarm = prewarm && !self.has_live_ide_session().await;
        let result = self
            .workspace
            .discover(&self.registry, &self.config.discovery);
        let detections = result.matches;
        for detection in &detections {
            self.register_detection(detection).await?;
        }
        if !result.complete {
            let broker = Arc::clone(self);
            tokio::spawn(async move {
                let mut config = broker.config.discovery.clone();
                config.max_initial_ms = config.max_initial_ms.max(30_000);
                let result = broker.workspace.discover(&broker.registry, &config);
                for detection in result.matches {
                    let _ = broker.register_detection(&detection).await;
                }
            });
        }
        if prewarm {
            for detection in detections {
                let _ = self.ensure_detection(&detection).await;
            }
        }
        Ok(())
    }

    async fn register_detection(&self, detection: &Detection) -> Result<ClientKey, ClspError> {
        let definition = self
            .registry
            .server(&detection.server_id)
            .ok_or_else(|| broker_error("detected server is not in the registry"))?
            .clone();
        if self
            .config
            .lsp
            .get(&definition.id)
            .and_then(|server| server.enabled)
            == Some(false)
        {
            return Err(ClspError::new(
                ErrorCode::ServerUnavailable,
                format!("{} is disabled", definition.id),
            ));
        }
        let override_config = self.config.lsp.get(&definition.id);
        let resolution_digest = resolution_fingerprint(
            &definition,
            &detection.root,
            override_config.and_then(|value| value.executable.as_deref()),
        );
        let config_digest = hex::encode(Sha256::digest(format!(
            "{}:{resolution_digest}",
            self.config_digest
        )));
        let key = ClientKey {
            root: detection.root.clone(),
            server_id: definition.id.clone(),
            artifact_version: recipe_version(&definition.install).into(),
            config_digest,
        };
        let mut servers = self.servers.write().await;
        if !servers.contains_key(&key) {
            servers.insert(
                key.clone(),
                Arc::new(ServerHandle {
                    ensure: Mutex::new(()),
                    inner: Mutex::new(ManagedServer {
                        key: key.clone(),
                        definition,
                        state: ServerState::Detected,
                        executable: None,
                        client: None,
                        detail: None,
                        install_progress: None,
                        failures: 0,
                        retry_after: None,
                        last_used: Instant::now(),
                    }),
                }),
            );
            drop(servers);
            self.publish(EventBody::ServerState {
                key: key.clone(),
                state: ServerState::Detected,
                detail: None,
            })
            .await;
        }
        Ok(key)
    }

    async fn ensure_detection(
        self: &Arc<Self>,
        detection: &Detection,
    ) -> Result<Arc<LspClient>, ClspError> {
        let key = self.register_detection(detection).await?;
        self.ensure_key(&key).await
    }

    async fn ensure_key(self: &Arc<Self>, key: &ClientKey) -> Result<Arc<LspClient>, ClspError> {
        let handle = self
            .servers
            .read()
            .await
            .get(key)
            .cloned()
            .ok_or_else(|| broker_error("server key is not registered"))?;
        let _work = WorkGuard::new(Arc::clone(&self.active_work));
        let _ensure = handle.ensure.lock().await;
        let (definition, server_key) = {
            let mut server = handle.inner.lock().await;
            server.last_used = Instant::now();
            if server
                .retry_after
                .is_some_and(|retry_after| retry_after > Instant::now())
            {
                return Err(ClspError::new(
                    ErrorCode::ServerUnavailable,
                    "language server restart backoff is active",
                )
                .for_server(&server.definition.id)
                .retryable());
            }
            if let Some(client) = server.client.clone() {
                if client.is_running().await {
                    return Ok(client);
                }
                server.client = None;
                let retry_seconds = mark_failure(&mut server);
                self.set_state(
                    &mut server,
                    ServerState::Failed,
                    Some(format!(
                        "language server process exited; retry in {retry_seconds}s"
                    )),
                )
                .await;
                return Err(ClspError::new(
                    ErrorCode::ServerUnavailable,
                    "language server process exited",
                )
                .for_server(&server.definition.id)
                .retryable());
            }
            self.set_state(&mut server, ServerState::Resolving, None)
                .await;
            (server.definition.clone(), server.key.clone())
        };
        let override_config = self.config.lsp.get(&definition.id);
        let explicit = override_config.and_then(|value| value.executable.clone());
        let install_broker = Arc::clone(self);
        let install_handle = Arc::clone(&handle);
        let install_server_id = definition.id.clone();
        let resolution = self
            .resolver
            .resolve_server(
                &definition,
                &server_key.root,
                explicit.as_deref(),
                move || async move {
                    let mut server = install_handle.inner.lock().await;
                    server.install_progress = Some(0.0);
                    install_broker
                        .set_state(&mut server, ServerState::Installing, None)
                        .await;
                    install_broker
                        .publish(EventBody::InstallProgress {
                            server_id: install_server_id,
                            progress: 0.0,
                        })
                        .await;
                },
            )
            .await;
        let resolution = match resolution {
            Ok(resolution) => resolution,
            Err(error) => {
                let mut server = handle.inner.lock().await;
                let state = if error.code == ErrorCode::RuntimeUnavailable {
                    ServerState::Blocked
                } else {
                    ServerState::Failed
                };
                if state == ServerState::Failed {
                    mark_failure(&mut server);
                }
                self.set_state(&mut server, state, Some(error.message.clone()))
                    .await;
                return Err(error);
            }
        };
        self.record_resolution(server_key.clone(), resolution.clone())
            .await;
        {
            let mut server = handle.inner.lock().await;
            if resolution.source == ExecutableSource::Installed {
                server.install_progress = Some(1.0);
                self.publish(EventBody::InstallProgress {
                    server_id: definition.id.clone(),
                    progress: 1.0,
                })
                .await;
            }
            server.executable = Some(resolution.path.clone());
            self.set_state(&mut server, ServerState::Starting, None)
                .await;
        }
        let client = async {
            let args = lsp_start_args(
                &definition.id,
                &definition.args,
                &server_key.root,
                &self.paths().workspace_state,
            )?;
            LspClient::start(LspStartOptions {
                server_id: &definition.id,
                executable: &resolution.path,
                args: &args,
                root: &server_key.root,
                workspace: self.workspace.clone(),
                request_timeout: Duration::from_secs(10),
                max_message_bytes: self.config.limits.max_response_bytes,
                max_file_bytes: self.config.limits.max_file_bytes,
                max_stderr_bytes: self.config.limits.max_stderr_bytes,
                max_diagnostics_per_file: self.config.diagnostics.max_per_file,
                npm_modules_root: resolution.npm_modules_root.as_deref(),
            })
            .await
        }
        .await;
        match client {
            Ok(client) => {
                let mut server = handle.inner.lock().await;
                server.client = Some(Arc::clone(&client));
                server.failures = 0;
                server.retry_after = None;
                self.set_state(&mut server, ServerState::Running, None)
                    .await;
                Ok(client)
            }
            Err(error) => {
                let mut server = handle.inner.lock().await;
                if error.code == ErrorCode::RuntimeUnavailable {
                    self.set_state(
                        &mut server,
                        ServerState::Blocked,
                        Some(error.message.clone()),
                    )
                    .await;
                } else {
                    let retry_seconds = mark_failure(&mut server);
                    self.set_state(
                        &mut server,
                        ServerState::Failed,
                        Some(format!("{}; retry in {retry_seconds}s", error.message)),
                    )
                    .await;
                }
                Err(error)
            }
        }
    }

    async fn ensure_file(
        self: &Arc<Self>,
        input: &Path,
    ) -> Result<Vec<(ServerDefinition, Arc<LspClient>)>, ClspError> {
        let path = self
            .workspace
            .resolve_file(input, self.config.limits.max_file_bytes)?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ClspError::new(
                    ErrorCode::UnsupportedFile,
                    "file has no supported extension",
                )
            })?;
        let definitions: Vec<_> = self
            .workspace
            .matching_servers(&path, extension, &self.registry)
            .into_iter()
            .cloned()
            .collect();
        if definitions.is_empty() {
            return Err(ClspError::new(
                ErrorCode::UnsupportedFile,
                "no Phase 1 server matches this file",
            )
            .for_path(path));
        }
        let mut clients = Vec::new();
        let mut last_error = None;
        for definition in definitions {
            let detection = Detection {
                server_id: definition.id.clone(),
                root: self.workspace.root_for_file(&path, &definition),
            };
            match self.ensure_detection(&detection).await {
                Ok(client) => clients.push((definition, client)),
                Err(error) => last_error = Some(error),
            }
        }
        if clients.is_empty() {
            return Err(
                last_error.unwrap_or_else(|| broker_error("no matching server is available"))
            );
        }
        Ok(clients)
    }

    async fn query(
        self: &Arc<Self>,
        mut request: QueryRequest,
    ) -> Result<crate::protocol::QueryResult, ClspError> {
        request.path = self
            .workspace
            .resolve_file(&request.path, self.config.limits.max_file_bytes)?;
        let clients = self.ensure_file(&request.path).await?;
        let mut last_error = None;
        for (definition, client) in clients {
            if let Err(error) = client
                .sync_file(&request.path, &definition.language_id)
                .await
            {
                last_error = Some(error);
                continue;
            }
            match client.query(request.clone()).await {
                Ok(result) => return Ok(result),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| broker_error("LSP query failed")))
    }

    async fn diagnostics(
        self: &Arc<Self>,
        inputs: &[PathBuf],
        minimum_severity: Option<crate::protocol::DiagnosticSeverity>,
        wait_ms: Option<u64>,
    ) -> Result<DiagnosticsReport, ClspError> {
        if inputs.is_empty() || inputs.len() > self.config.diagnostics.max_files {
            return Err(ClspError::new(
                ErrorCode::InvalidRequest,
                "diagnostics path count is outside configured bounds",
            ));
        }
        let wait = Duration::from_millis(
            wait_ms
                .unwrap_or(self.config.diagnostics.wait_ms)
                .min(self.config.diagnostics.wait_ms),
        );
        let mut diagnostics = Vec::new();
        let mut sources = Vec::new();
        let mut baseline_available = true;
        for input in inputs {
            let path = self
                .workspace
                .resolve_file(input, self.config.limits.max_file_bytes)?;
            for (definition, client) in self.ensure_file(&path).await? {
                let _ = client.sync_file(&path, &definition.language_id).await?;
                let report = client.diagnostics(std::slice::from_ref(&path), wait).await;
                diagnostics.extend(report.diagnostics);
                sources.extend(report.sources);
                baseline_available &= report.baseline_available;
            }
        }
        let fresh = !sources.is_empty() && sources.iter().all(|source| source.fresh);
        let minimum_severity = minimum_severity.unwrap_or(self.config.diagnostics.minimum_severity);
        diagnostics.retain(|diagnostic| {
            severity_rank(diagnostic.severity) <= severity_rank(minimum_severity)
        });
        Ok(DiagnosticsReport {
            diagnostics,
            fresh,
            sources,
            baseline_available,
        })
    }

    async fn sync_files(
        self: &Arc<Self>,
        inputs: &[PathBuf],
        preserve_for_hook: bool,
    ) -> Result<RpcResponse, ClspError> {
        if inputs.len() > self.config.diagnostics.max_files {
            return Err(ClspError::new(
                ErrorCode::InvalidRequest,
                "sync path count is outside configured bounds",
            ));
        }
        let mut paths = Vec::new();
        let mut new_errors = Vec::new();
        let mut fresh = true;
        let mut baseline_available = true;
        let reuse_ide = preserve_for_hook && self.has_live_ide_session().await;
        for input in inputs {
            let path = self
                .workspace
                .resolve_file(input, self.config.limits.max_file_bytes)?;
            paths.push(path.clone());
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            let definitions: Vec<_> = self
                .workspace
                .matching_servers(&path, extension, &self.registry)
                .into_iter()
                .cloned()
                .collect();
            for definition in definitions {
                let detection = Detection {
                    server_id: definition.id.clone(),
                    root: self.workspace.root_for_file(&path, &definition),
                };
                let key = self.register_detection(&detection).await?;
                let handle = self.servers.read().await.get(&key).cloned();
                let running = if let Some(handle) = handle {
                    handle.inner.lock().await.client.clone()
                } else {
                    None
                };
                let Some(client) = running else {
                    if reuse_ide {
                        fresh = false;
                        baseline_available = false;
                        continue;
                    }
                    let broker = Arc::clone(self);
                    tokio::spawn(async move {
                        let _ = broker.ensure_detection(&detection).await;
                    });
                    fresh = false;
                    baseline_available = false;
                    continue;
                };
                let sync = client.sync_file(&path, &definition.language_id).await?;
                let baseline_key = (key, path.clone());
                let mut baselines = self.watcher_baselines.lock().await;
                let sync = handoff_watcher_baseline(
                    &mut baselines,
                    baseline_key,
                    sync,
                    preserve_for_hook,
                    self.config.diagnostics.max_files * self.registry.server.len(),
                );
                drop(baselines);
                if let Some(sync) = sync {
                    let report = client
                        .diagnostics(
                            std::slice::from_ref(&path),
                            Duration::from_millis(self.config.diagnostics.wait_ms),
                        )
                        .await;
                    fresh &= report.fresh;
                    baseline_available &= sync.baseline_available;
                    if sync.baseline_available {
                        new_errors.extend(client.diagnostics_store().new_errors(&sync).await);
                    }
                }
            }
        }
        Ok(RpcResponse::Sync {
            paths,
            new_errors,
            fresh,
            baseline_available,
        })
    }

    async fn note_watcher_changes(&self, paths: &[PathBuf]) {
        let mut changes = self.watcher_changes.lock().await;
        for path in paths.iter().filter(|path| path.is_file()) {
            changes.insert(path.clone());
        }
        while changes.len() > self.config.diagnostics.max_files * 4 {
            changes.pop_first();
        }
    }

    async fn merge_watcher_changes(&self, mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut changes = self.watcher_changes.lock().await;
        for path in std::mem::take(&mut *changes) {
            if paths.len() >= self.config.diagnostics.max_files {
                break;
            }
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths.truncate(self.config.diagnostics.max_files);
        paths
    }

    async fn stop_key(&self, key: &ClientKey) -> Result<(), ClspError> {
        let handle = self
            .servers
            .read()
            .await
            .get(key)
            .cloned()
            .ok_or_else(|| broker_error("server key is not registered"))?;
        let _ensure = handle.ensure.lock().await;
        let mut server = handle.inner.lock().await;
        if let Some(client) = server.client.take() {
            self.set_state(&mut server, ServerState::Stopping, None)
                .await;
            let _ = client.shutdown().await;
        }
        self.set_state(&mut server, ServerState::Stopped, None)
            .await;
        Ok(())
    }

    async fn clear_retry(&self, key: &ClientKey) -> Result<(), ClspError> {
        let handle = self
            .servers
            .read()
            .await
            .get(key)
            .cloned()
            .ok_or_else(|| broker_error("server key is not registered"))?;
        let mut server = handle.inner.lock().await;
        server.failures = 0;
        server.retry_after = None;
        Ok(())
    }

    async fn record_resolution(&self, key: ClientKey, resolution: ResolvedExecutable) {
        let mut resolutions = self.resolutions.lock().await;
        resolutions.insert(key, resolution);
        if let Err(error) = self.resolver.write_workspace_lock(resolutions.iter()).await {
            drop(resolutions);
            self.publish(EventBody::BrokerMessage {
                message: format!("cannot update lsp.lock: {}", error.message),
            })
            .await;
        }
    }

    async fn set_state(
        &self,
        server: &mut ManagedServer,
        state: ServerState,
        detail: Option<String>,
    ) {
        server.state = state;
        server.detail = detail.clone();
        if state != ServerState::Installing {
            server.install_progress = None;
        }
        server.last_used = Instant::now();
        self.publish(EventBody::ServerState {
            key: server.key.clone(),
            state,
            detail,
        })
        .await;
    }

    async fn publish(&self, body: EventBody) {
        let event = BrokerEvent {
            seq: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            timestamp_ms: now_ms(),
            body,
        };
        let mut events = self.events.lock().await;
        events.push_back(event.clone());
        while events.len() > EVENT_RING_CAPACITY {
            events.pop_front();
        }
        append_event_log(&self.event_log, &event).await;
        drop(events);
        let _ = self.event_tx.send(event);
    }

    async fn events_after(&self, sequence: u64) -> Vec<BrokerEvent> {
        self.events
            .lock()
            .await
            .iter()
            .filter(|event| event.seq > sequence)
            .cloned()
            .collect()
    }

    async fn snapshot(&self) -> BrokerSnapshot {
        let handles: Vec<_> = self.servers.read().await.values().cloned().collect();
        let mut servers = Vec::with_capacity(handles.len());
        for handle in handles {
            let mut server = handle.inner.lock().await;
            if let Some(client) = server.client.clone()
                && !client.is_running().await
            {
                server.client = None;
                let retry_seconds = mark_failure(&mut server);
                self.set_state(
                    &mut server,
                    ServerState::Failed,
                    Some(format!(
                        "language server process exited; retry in {retry_seconds}s"
                    )),
                )
                .await;
            }
            servers.push(ServerSnapshot {
                key: server.key.clone(),
                state: server.state,
                executable: server.executable.clone(),
                pid: server.client.as_ref().and_then(|client| client.pid()),
                detail: server.detail.clone(),
                install_progress: server.install_progress,
            });
        }
        let hook_last_seen_ms = self.hook_last_seen_ms.load(Ordering::Relaxed);
        let events = self.events.lock().await.iter().cloned().collect();
        let active_ide_sessions = {
            let mut ide = self.ide.lock().await;
            ide.sweep(Instant::now());
            ide.sessions.len()
        };
        BrokerSnapshot {
            protocol: PROTOCOL_VERSION.into(),
            workspace: self.workspace.root().to_path_buf(),
            broker_pid: std::process::id(),
            sequence: self.sequence.load(Ordering::Relaxed),
            servers,
            active_connections: self.connections.load(Ordering::Relaxed),
            active_leases: self.leases.lock().await.leases.len(),
            active_ide_sessions,
            hook_last_seen_ms: (hook_last_seen_ms != 0).then_some(hook_last_seen_ms),
            hook_same_turn_ready: hook_last_seen_ms != 0
                && now_ms().saturating_sub(hook_last_seen_ms)
                    <= self.config.lifecycle.session_lease_seconds * 1_000,
            recent_events: events,
        }
    }

    async fn sweep(self: &Arc<Self>) {
        let expired = self.leases.lock().await.sweep(Instant::now());
        for session_id in expired {
            self.publish(EventBody::LeaseChanged {
                session_id,
                active: false,
            })
            .await;
        }
        self.ide.lock().await.sweep(Instant::now());
        let handles: Vec<_> = self.servers.read().await.values().cloned().collect();
        let server_idle = Duration::from_secs(self.config.lifecycle.server_idle_seconds);
        for handle in handles {
            let mut server = handle.inner.lock().await;
            if server.client.is_some() && server.last_used.elapsed() >= server_idle {
                let client = server.client.take().expect("checked above");
                self.set_state(
                    &mut server,
                    ServerState::Stopping,
                    Some("idle timeout".into()),
                )
                .await;
                let _ = client.shutdown().await;
                self.set_state(
                    &mut server,
                    ServerState::Stopped,
                    Some("idle timeout".into()),
                )
                .await;
            }
        }
        let no_leases = self.leases.lock().await.leases.is_empty();
        let no_ide_sessions = self.ide.lock().await.sessions.is_empty();
        let idle = now_ms().saturating_sub(self.last_activity_ms.load(Ordering::Relaxed))
            >= self.config.lifecycle.broker_idle_seconds * 1_000;
        if no_leases
            && no_ide_sessions
            && self.connections.load(Ordering::Relaxed) == 0
            && self.active_work.load(Ordering::Relaxed) == 0
            && idle
            && !self.shutting_down.swap(true, Ordering::Relaxed)
        {
            self.shutdown.notify_waiters();
        }
    }

    fn touch(&self) {
        self.last_activity_ms.store(now_ms(), Ordering::Relaxed);
    }

    fn subscribe(&self) -> broadcast::Receiver<BrokerEvent> {
        self.event_tx.subscribe()
    }

    async fn stop_all(&self) {
        let handles: Vec<_> = self.servers.read().await.values().cloned().collect();
        for handle in handles {
            let mut server = handle.inner.lock().await;
            if let Some(client) = server.client.take() {
                let _ = client.shutdown().await;
            }
            server.state = ServerState::Stopped;
        }
    }
}

fn ide_diagnostic_to_protocol(
    diagnostic: IdeDiagnostic,
    text: &str,
) -> Result<Diagnostic, ClspError> {
    Ok(Diagnostic {
        path: diagnostic.path,
        range: TextRange {
            start: lsp_to_external(
                text,
                diagnostic.range.start.line,
                diagnostic.range.start.character,
                PositionEncoding::Utf16,
            )?,
            end: lsp_to_external(
                text,
                diagnostic.range.end.line,
                diagnostic.range.end.character,
                PositionEncoding::Utf16,
            )?,
        },
        severity: diagnostic.severity,
        code: diagnostic.code,
        source: diagnostic.source,
        message: diagnostic.message,
        server_id: "vscode_problems".into(),
    })
}

pub async fn run(workspace_path: &Path, prewarm_on_start: bool) -> Result<(), ClspError> {
    let workspace = Workspace::open(workspace_path)?;
    let config = Config::load(workspace.root(), ConfigOverrides::default())?;
    config.ensure_enabled()?;
    let registry = Registry::builtin()?;
    let paths = StatePaths::for_workspace(&workspace.hash())?;
    let metadata_path = paths.workspace_state.join("broker.json");
    let broker = Broker::new(config, workspace.clone(), registry, paths)?;
    let name = pipe_name(&workspace)?;
    let mut listener = create_pipe_server(&name, true)?;
    let metadata = BrokerMetadata::new(name.clone(), workspace.root().to_path_buf())?;
    publish_metadata(&metadata_path, &metadata).await?;
    apply_user_system_dacl(&broker.paths().logs, true)?;
    let _watcher = start_watcher(Arc::clone(&broker))?;

    let discovery = Arc::clone(&broker);
    tokio::spawn(async move {
        let _ = discovery
            .discover_and_register(prewarm_on_start && discovery.config.prewarm)
            .await;
    });
    let lifecycle = Arc::clone(&broker);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            lifecycle.sweep().await;
            if lifecycle.shutting_down.load(Ordering::Relaxed) {
                return;
            }
        }
    });

    loop {
        tokio::select! {
            result = listener.connect() => {
                result.map_err(broker_error)?;
                let connected = listener;
                listener = create_pipe_server(&name, false)?;
                let broker = Arc::clone(&broker);
                let token = metadata.token.clone();
                tokio::spawn(async move {
                    handle_connection(broker, connected, token).await;
                });
            }
            _ = broker.shutdown.notified() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    broker.stop_all().await;
    let _ = tokio::fs::remove_file(metadata_path).await;
    Ok(())
}

fn start_watcher(broker: Arc<Broker>) -> Result<RecommendedWatcher, ClspError> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let _ = sender.send(result);
    })
    .map_err(broker_error)?;
    watcher
        .watch(broker.workspace.root(), RecursiveMode::Recursive)
        .map_err(broker_error)?;
    tokio::spawn(async move {
        while let Some(result) = receiver.recv().await {
            let Ok(event) = result else { continue };
            if event.paths.iter().any(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(".clsp.toml"))
            }) {
                if !broker.shutting_down.swap(true, Ordering::Relaxed) {
                    broker.shutdown.notify_waiters();
                }
                return;
            }
            let paths: Vec<_> = event
                .paths
                .into_iter()
                .take(broker.config.diagnostics.max_files)
                .collect();
            if !paths.is_empty() {
                broker.note_watcher_changes(&paths).await;
                let _ = broker.sync_files(&paths, true).await;
            }
        }
    });
    Ok(watcher)
}

async fn handle_connection(broker: Arc<Broker>, mut pipe: NamedPipeServer, token: String) {
    let max_frame_bytes = broker.config.limits.max_response_bytes;
    if authenticate_server(&mut pipe, &token, max_frame_bytes)
        .await
        .is_err()
    {
        return;
    }
    broker.connections.fetch_add(1, Ordering::Relaxed);
    let _connection = ConnectionGuard(Arc::clone(&broker));
    loop {
        let message = match read_wire(&mut pipe, max_frame_bytes).await {
            Ok(message) => message,
            Err(_) => return,
        };
        let WireMessage::Request(request) = message else {
            return;
        };
        let subscribe_after = match &request.body {
            RpcRequest::Subscribe { after_seq } => Some(*after_seq),
            _ => None,
        };
        let mut subscription = subscribe_after.map(|_| broker.subscribe());
        let result = broker.handle(request.body).await;
        let response = match result {
            Ok(result) => ResponseEnvelope {
                id: request.id,
                result: Some(result),
                error: None,
            },
            Err(error) => ResponseEnvelope {
                id: request.id,
                result: None,
                error: Some(error),
            },
        };
        if write_wire(&mut pipe, &WireMessage::Response(response), max_frame_bytes)
            .await
            .is_err()
        {
            return;
        }
        if let Some(events) = subscription.as_mut() {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(_) => return,
                };
                if write_wire(
                    &mut pipe,
                    &WireMessage::Event(EventEnvelope {
                        seq: event.seq,
                        timestamp_ms: event.timestamp_ms,
                        body: event.body,
                    }),
                    max_frame_bytes,
                )
                .await
                .is_err()
                {
                    return;
                }
            }
        }
    }
}

struct ConnectionGuard(Arc<Broker>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
        self.0.touch();
    }
}

struct WorkGuard(Arc<AtomicUsize>);

impl WorkGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn recipe_version(recipe: &InstallRecipe) -> &str {
    match recipe {
        InstallRecipe::Npm { version, .. }
        | InstallRecipe::Command { version, .. }
        | InstallRecipe::GithubZip { version, .. }
        | InstallRecipe::Manual { version, .. } => version,
    }
}

fn severity_rank(severity: crate::protocol::DiagnosticSeverity) -> u8 {
    match severity {
        crate::protocol::DiagnosticSeverity::Error => 1,
        crate::protocol::DiagnosticSeverity::Warning => 2,
        crate::protocol::DiagnosticSeverity::Information => 3,
        crate::protocol::DiagnosticSeverity::Hint => 4,
    }
}

fn validate_correlation_id(name: &str, value: &str) -> Result<(), ClspError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ClspError::new(
            ErrorCode::InvalidRequest,
            format!("{name} ID is outside supported bounds"),
        ));
    }
    Ok(())
}

fn correlation_hash(codex_session_id: &str, tool_use_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"clsp-ide-review-v1\0");
    digest.update(codex_session_id.as_bytes());
    digest.update(b"\0");
    digest.update(tool_use_id.as_bytes());
    hex::encode(digest.finalize())
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn lsp_start_args(
    server_id: &str,
    args: &[String],
    root: &Path,
    workspace_state: &Path,
) -> Result<Vec<String>, ClspError> {
    let mut args = args.to_vec();
    if server_id == FSHARP_SERVER_ID {
        let state_directory = workspace_state
            .join("lsp/fsharp")
            .join(hash_bytes(root.to_string_lossy().as_bytes()));
        fs::create_dir_all(&state_directory).map_err(|error| {
            ClspError::new(
                ErrorCode::ServerUnavailable,
                format!("cannot create FsAutoComplete state directory: {error}"),
            )
            .for_server(server_id)
            .retryable()
        })?;
        args.push("--state-directory".into());
        args.push(state_directory.to_string_lossy().into_owned());
    }
    if server_id == JDTLS_SERVER_ID {
        let data_directory = workspace_state
            .join("lsp/jdtls")
            .join(hash_bytes(root.to_string_lossy().as_bytes()))
            .join("data");
        fs::create_dir_all(&data_directory).map_err(|error| {
            ClspError::new(
                ErrorCode::ServerUnavailable,
                format!("cannot create JDTLS data directory: {error}"),
            )
            .for_server(server_id)
            .retryable()
        })?;
        args.push("-data".into());
        args.push(data_directory.to_string_lossy().into_owned());
    }
    Ok(args)
}

fn review_file_name(index: usize, side: &str, path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 16
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    format!("{index:03}-{side}{extension}")
}

fn review_child(directory: &Path, name: &str) -> Option<PathBuf> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Some(directory.join(name)),
        _ => None,
    }
}

fn write_review_file(path: &Path, bytes: &[u8]) -> Result<(), ClspError> {
    fs::write(path, bytes).map_err(broker_error)?;
    apply_user_system_dacl(path, false)
}

fn read_review_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ClspError> {
    let metadata = fs::metadata(path).map_err(broker_error)?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(ClspError::new(
            ErrorCode::InvalidRequest,
            "IDE review file is not a bounded regular file",
        ));
    }
    let bytes = fs::read(path).map_err(broker_error)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ClspError::new(
            ErrorCode::InvalidRequest,
            "IDE review file exceeds its limit",
        ));
    }
    Ok(bytes)
}

fn cleanup_ide_reviews(root: &Path) {
    if verify_user_system_dacl(root).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() || verify_user_system_dacl(&path).is_err()
        {
            continue;
        }
        let old_enough = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= IDE_REVIEW_TTL);
        if old_enough {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn handoff_watcher_baseline(
    baselines: &mut BTreeMap<(ClientKey, PathBuf), SyncResult>,
    key: (ClientKey, PathBuf),
    current: Option<SyncResult>,
    preserve_for_hook: bool,
    capacity: usize,
) -> Option<SyncResult> {
    if preserve_for_hook {
        if let Some(current) = current {
            baselines.insert(key, current);
            while baselines.len() > capacity {
                baselines.pop_first();
            }
        }
        None
    } else if current.is_some() {
        baselines.remove(&key);
        current
    } else {
        baselines.remove(&key)
    }
}

fn mark_failure(server: &mut ManagedServer) -> u64 {
    server.failures = server.failures.saturating_add(1);
    let seconds = retry_delay(server.failures);
    server.retry_after = Some(Instant::now() + Duration::from_secs(seconds));
    seconds
}

fn retry_delay(failures: u32) -> u64 {
    1u64 << failures.saturating_sub(1).min(5)
}

fn validate_session_id(session_id: &str) -> Result<(), ClspError> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(ClspError::new(
            ErrorCode::InvalidRequest,
            "session_id is invalid",
        ));
    }
    Ok(())
}

fn validate_ide_session_id(session_id: &str) -> Result<(), ClspError> {
    if !valid_ide_session_id(session_id) {
        return Err(ClspError::new(
            ErrorCode::InvalidRequest,
            "IDE session ID must be 64 hexadecimal characters",
        ));
    }
    Ok(())
}

fn valid_ide_session_id(session_id: &str) -> bool {
    session_id.len() == IDE_SESSION_ID_HEX_LEN
        && session_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ide_unavailable() -> ClspError {
    ClspError::new(
        ErrorCode::IdeUnavailable,
        "matching live VS Code session is unavailable",
    )
    .retryable()
}

fn ide_result_error(result: IdeActionResult) -> ClspError {
    match result {
        IdeActionResult::Error { message } => {
            ClspError::new(ErrorCode::IdeUnavailable, message).retryable()
        }
        _ => ClspError::new(
            ErrorCode::InvalidRequest,
            "VS Code adapter returned an unexpected action result",
        ),
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn broker_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::BrokerUnavailable, error.to_string()).retryable()
}

async fn append_event_log(path: &Path, event: &BrokerEvent) {
    let Ok(mut line) = serde_json::to_vec(event) else {
        return;
    };
    line.push(b'\n');
    let truncate = tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.len() >= EVENT_LOG_MAX_BYTES);
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if truncate {
        options.truncate(true);
    } else {
        options.append(true);
    }
    if let Ok(mut file) = options.open(path).await {
        let _ = file.write_all(&line).await;
    }
}

#[cfg(test)]
mod tests {
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
    fn expired_leases_do_not_require_session_end() {
        let now = Instant::now();
        let mut leases = LeaseBook::default();
        leases.renew("alive".into(), Duration::from_secs(10), now);
        leases.renew("expired".into(), Duration::from_secs(1), now);
        let expired = leases.sweep(now + Duration::from_secs(2));
        assert_eq!(expired, vec!["expired"]);
        assert!(leases.leases.contains_key("alive"));
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

        assert!(
            handoff_watcher_baseline(&mut baselines, key.clone(), Some(sync), true, 8).is_none()
        );
        let consumed =
            handoff_watcher_baseline(&mut baselines, key.clone(), None, false, 8).unwrap();
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
                diagnostic_baselines: BTreeMap::new(),
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
        } = sync_with_ide_diagnostics(&broker, &session_id, &file, "new-error", vec![old, new])
            .await
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
        let RpcResponse::IdeDiagnostics(report) = diagnostics_request.await.unwrap().unwrap()
        else {
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
}

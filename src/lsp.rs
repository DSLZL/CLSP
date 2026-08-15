use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, Notify, RwLock, mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use url::Url;

use crate::{
    edit_diagnostics::{
        Baseline, DiagnosticSnapshot, HOOK_MAX_ERRORS, LspDiagnosticSource, Verification,
        diagnostic_key as edit_diagnostic_key, verify as verify_edit,
    },
    installer::{
        jdtls_extension_layout, jdtls_java_for_launcher, julials_extension_environment,
        sanitize_command, yaml_extension_l10n,
    },
    protocol::{
        ClspError, Diagnostic, DiagnosticSeverity, DiagnosticsReport, ErrorCode, Location,
        Position, QueryOperation, QueryRequest, QueryResult, SourceFreshness, TextRange,
    },
    setup::child_process_path,
    workspace::Workspace,
};

const HEADER_LIMIT: usize = 8 * 1024;
const MAX_HOVER_CHARS: usize = 64 * 1024;
const MAX_LOCATIONS: usize = 100;
const ASTRO_SERVER_ID: &str = "astro";
const CLOJURE_SERVER_ID: &str = "clojure-lsp";
const ELIXIR_LS_SERVER_ID: &str = "elixir-ls";
const DENO_SERVER_ID: &str = "deno";
const ESLINT_SERVER_ID: &str = "eslint";
const FSHARP_SERVER_ID: &str = "fsharp";
const INTELEPHENSE_SERVER_ID: &str = "intelephense";
const PRISMA_SERVER_ID: &str = "prisma";
const PYRIGHT_SERVER_ID: &str = "pyright";
const RUBY_LSP_SERVER_ID: &str = "ruby-lsp";
const JDTLS_SERVER_ID: &str = "jdtls";
const JULIALS_SERVER_ID: &str = "julials";
const KOTLIN_LS_SERVER_ID: &str = "kotlin-ls";
const SOURCEKIT_LSP_SERVER_ID: &str = "sourcekit-lsp";
const SVELTE_SERVER_ID: &str = "svelte";
const TERRAFORM_SERVER_ID: &str = "terraform";
const TYPESCRIPT_SERVER_ID: &str = "typescript";
const VUE_SERVER_ID: &str = "vue";
const YAML_LS_SERVER_ID: &str = "yaml-ls";
const SLOW_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SLOW_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(300);

type PendingRequests = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, ClspError>>>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
    Utf32,
}

impl PositionEncoding {
    fn from_initialize(value: &Value) -> Self {
        match value
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
        {
            Some("utf-8") => Self::Utf8,
            Some("utf-32") => Self::Utf32,
            _ => Self::Utf16,
        }
    }
}

#[derive(Clone, Debug)]
struct OpenDocument {
    uri: String,
    text: String,
    version: i32,
}

#[derive(Clone, Debug)]
pub struct SyncResult {
    pub path: PathBuf,
    pub version: i32,
    pub baseline: BTreeSet<String>,
    pub baseline_available: bool,
}

#[derive(Clone, Debug, Default)]
struct DiagnosticRecord {
    diagnostics: Vec<Diagnostic>,
    push_generation: u64,
    synchronized_version: Option<i32>,
    received_version: Option<i32>,
    fresh: bool,
    truncated: bool,
    reason: Option<String>,
}

#[derive(Default)]
pub struct DiagnosticsStore {
    records: Mutex<HashMap<PathBuf, DiagnosticRecord>>,
    changed: Notify,
}

impl DiagnosticsStore {
    pub async fn begin_sync(&self, path: &Path, version: i32) -> (BTreeSet<String>, bool) {
        let mut records = self.records.lock().await;
        let baseline_available = records.get(path).is_some_and(|record| !record.truncated);
        let record = records.entry(path.to_path_buf()).or_default();
        let baseline = record.diagnostics.iter().map(edit_diagnostic_key).collect();
        record.synchronized_version = Some(version);
        record.fresh = false;
        record.truncated = false;
        record.reason = Some("awaiting_diagnostics".into());
        (baseline, baseline_available)
    }

    pub async fn publish(&self, path: PathBuf, version: Option<i32>, diagnostics: Vec<Diagnostic>) {
        self.publish_with_truncation(path, version, diagnostics, false)
            .await;
    }

    async fn publish_with_truncation(
        &self,
        path: PathBuf,
        version: Option<i32>,
        diagnostics: Vec<Diagnostic>,
        truncated: bool,
    ) {
        let mut records = self.records.lock().await;
        let record = records.entry(path).or_default();
        record.push_generation = record.push_generation.saturating_add(1);
        let expected = record.synchronized_version;
        match (version, expected) {
            (Some(actual), Some(expected)) if actual == expected => {
                record.diagnostics = dedupe_diagnostics(diagnostics);
                record.received_version = Some(actual);
                record.truncated = truncated;
                record.fresh = !truncated;
                record.reason = truncated.then_some("diagnostics_truncated".into());
            }
            (Some(actual), Some(expected)) if actual > expected => {
                record.fresh = false;
                record.truncated = truncated;
                record.reason = Some("future_document_version".into());
            }
            (Some(actual), _) => {
                record.diagnostics = dedupe_diagnostics(diagnostics);
                record.received_version = Some(actual);
                record.truncated = truncated;
                record.fresh = false;
                record.reason = Some("stale_document_version".into());
            }
            (None, _) => {
                record.diagnostics = dedupe_diagnostics(diagnostics);
                record.received_version = None;
                record.truncated = truncated;
                record.fresh = false;
                record.reason = Some("diagnostic_version_unavailable".into());
            }
        }
        drop(records);
        self.changed.notify_waiters();
    }

    pub async fn publish_pull(&self, path: PathBuf, version: i32, diagnostics: Vec<Diagnostic>) {
        self.publish_pull_with_truncation(path, version, diagnostics, false)
            .await;
    }

    async fn publish_pull_with_truncation(
        &self,
        path: PathBuf,
        version: i32,
        diagnostics: Vec<Diagnostic>,
        truncated: bool,
    ) {
        let mut records = self.records.lock().await;
        let record = records.entry(path).or_default();
        if record.synchronized_version == Some(version) {
            record.diagnostics = dedupe_diagnostics(diagnostics);
            record.received_version = Some(version);
            record.truncated = truncated;
            record.fresh = !truncated;
            record.reason = truncated.then_some("diagnostics_truncated".into());
        }
        drop(records);
        self.changed.notify_waiters();
    }

    pub async fn report(
        &self,
        server_id: &str,
        paths: &[PathBuf],
        wait: Duration,
        max_per_file: usize,
    ) -> DiagnosticsReport {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let report = self.snapshot(server_id, paths, max_per_file).await;
            if report.fresh || tokio::time::Instant::now() >= deadline {
                return mark_diagnostics_timeout(report);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if timeout(remaining, self.changed.notified()).await.is_err() {
                return mark_diagnostics_timeout(
                    self.snapshot(server_id, paths, max_per_file).await,
                );
            }
        }
    }

    pub async fn new_errors(&self, sync: &SyncResult) -> Vec<Diagnostic> {
        let records = self.records.lock().await;
        let Some(record) = records.get(&sync.path) else {
            return Vec::new();
        };
        let baseline = Baseline::from_keys(
            BTreeSet::from([sync.path.clone()]),
            sync.baseline.clone(),
            sync.baseline_available,
        );
        let source = DiagnosticSnapshot::new(
            BTreeSet::from([sync.path.clone()]),
            record.diagnostics.clone(),
            record.fresh,
            !record.truncated,
            record.truncated,
        );
        verify_edit(
            Some(&baseline),
            &BTreeSet::from([sync.path.clone()]),
            &source,
            HOOK_MAX_ERRORS,
        )
        .new_errors
    }

    pub(crate) async fn verify(
        &self,
        sync: &SyncResult,
        report: &DiagnosticsReport,
        max_errors: usize,
    ) -> Verification {
        let baseline = Baseline::from_keys(
            BTreeSet::from([sync.path.clone()]),
            sync.baseline.clone(),
            sync.baseline_available,
        );
        let source = LspDiagnosticSource::new(std::slice::from_ref(&sync.path), report);
        verify_edit(
            Some(&baseline),
            &BTreeSet::from([sync.path.clone()]),
            &source,
            max_errors,
        )
    }

    async fn snapshot(
        &self,
        server_id: &str,
        paths: &[PathBuf],
        max_per_file: usize,
    ) -> DiagnosticsReport {
        let records = self.records.lock().await;
        let mut diagnostics = Vec::new();
        let mut sources = Vec::new();
        for path in paths {
            let Some(record) = records.get(path) else {
                sources.push(SourceFreshness {
                    server_id: server_id.into(),
                    fresh: false,
                    reason: Some("no_diagnostics_received".into()),
                    document_version: None,
                });
                continue;
            };
            diagnostics.extend(record.diagnostics.iter().take(max_per_file).cloned());
            sources.push(SourceFreshness {
                server_id: server_id.into(),
                fresh: record.fresh && !record.truncated,
                reason: if record.truncated {
                    Some("diagnostics_truncated".into())
                } else {
                    record.reason.clone()
                },
                document_version: record.received_version,
            });
        }
        let fresh = !sources.is_empty() && sources.iter().all(|source| source.fresh);
        DiagnosticsReport {
            diagnostics,
            fresh,
            sources,
            baseline_available: paths
                .iter()
                .all(|path| records.get(path).is_some_and(|record| !record.truncated)),
        }
    }
}

fn mark_diagnostics_timeout(mut report: DiagnosticsReport) -> DiagnosticsReport {
    if !report.fresh {
        for source in &mut report.sources {
            if matches!(
                source.reason.as_deref(),
                Some("awaiting_diagnostics" | "no_diagnostics_received")
            ) {
                source.reason = Some("diagnostics_timeout".into());
            }
        }
    }
    report
}

pub struct LspClient {
    server_id: String,
    root: PathBuf,
    workspace: Workspace,
    writer: Arc<Mutex<ChildStdin>>,
    child: Mutex<Child>,
    pid: Option<u32>,
    pending: PendingRequests,
    next_id: AtomicU64,
    documents: Arc<Mutex<HashMap<PathBuf, OpenDocument>>>,
    diagnostics: Arc<DiagnosticsStore>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    encoding: Arc<RwLock<PositionEncoding>>,
    supports_pull_diagnostics: RwLock<bool>,
    request_timeout: Duration,
    max_message_bytes: usize,
    max_file_bytes: u64,
    max_diagnostics_per_file: usize,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

pub struct LspStartOptions<'a> {
    pub server_id: &'a str,
    pub executable: &'a Path,
    pub args: &'a [String],
    pub root: &'a Path,
    pub workspace: Workspace,
    pub request_timeout: Duration,
    pub max_message_bytes: usize,
    pub max_file_bytes: u64,
    pub max_stderr_bytes: usize,
    pub max_diagnostics_per_file: usize,
    pub npm_modules_root: Option<&'a Path>,
}

impl LspClient {
    pub async fn start(options: LspStartOptions<'_>) -> Result<Arc<Self>, ClspError> {
        let initialization_options = server_initialization_options(
            options.server_id,
            options.root,
            options.workspace.root(),
            options.executable,
            options.npm_modules_root,
        )?;
        let runtime_args = server_runtime_args(
            options.server_id,
            options.root,
            options.workspace.root(),
            options.executable,
            options.npm_modules_root,
        )?;
        let mut command = if uses_jdtls_java_host(options.server_id, options.executable) {
            jdtls_java_command(options.executable, options.root, options.request_timeout).await?
        } else if options.server_id == JULIALS_SERVER_ID
            && options.executable.file_name() == Some(std::ffi::OsStr::new("Project.toml"))
        {
            julials_extension_command(options.executable)?
        } else if uses_node_host(options.server_id, options.executable) {
            let name = match options.server_id {
                ESLINT_SERVER_ID => "ESLint Language Server",
                INTELEPHENSE_SERVER_ID => "PHP Intelephense",
                PRISMA_SERVER_ID => "Prisma Language Server",
                PYRIGHT_SERVER_ID => "Pyright",
                SVELTE_SERVER_ID => "Svelte Language Server",
                VUE_SERVER_ID => "Vue Language Server",
                YAML_LS_SERVER_ID => "YAML Language Server",
                _ => unreachable!(),
            };
            let node = which::which("node")
                .map_err(|_| runtime_error(format!("{name} requires Node.js")))?;
            let mut command = Command::new(node);
            command.arg(options.executable);
            command
        } else if uses_dotnet_host(options.server_id, options.executable) {
            let dotnet = which::which("dotnet")
                .map_err(|_| runtime_error("FsAutoComplete requires the .NET runtime"))?;
            let mut command = Command::new(dotnet);
            command.arg(options.executable);
            command
        } else {
            Command::new(options.executable)
        };
        command
            .args(runtime_args)
            .args(options.args)
            .current_dir(options.root);
        sanitize_command(&mut command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(server_error)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| server_error("missing LSP stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| server_error("missing LSP stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| server_error("missing LSP stderr"))?;
        let pid = child.id();
        let root_uri = path_to_uri(options.root)?;
        let root_name = options
            .root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace")
            .to_owned();
        let (diagnostic_refresh_tx, mut diagnostic_refresh_rx) = mpsc::channel(1);
        let client = Arc::new(Self {
            server_id: options.server_id.into(),
            root: options.root.to_path_buf(),
            workspace: options.workspace,
            writer: Arc::new(Mutex::new(stdin)),
            child: Mutex::new(child),
            pid,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            documents: Arc::new(Mutex::new(HashMap::new())),
            diagnostics: Arc::new(DiagnosticsStore::default()),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
            encoding: Arc::new(RwLock::new(PositionEncoding::Utf16)),
            supports_pull_diagnostics: RwLock::new(false),
            request_timeout: options.request_timeout,
            max_message_bytes: options.max_message_bytes,
            max_file_bytes: options.max_file_bytes,
            max_diagnostics_per_file: options.max_diagnostics_per_file,
            tasks: Mutex::new(Vec::new()),
        });

        client
            .spawn_owned(reader_loop(
                stdout,
                Arc::clone(&client.writer),
                Arc::clone(&client.pending),
                Arc::clone(&client.documents),
                Arc::clone(&client.diagnostics),
                client.workspace.clone(),
                client.server_id.clone(),
                options.root.to_path_buf(),
                options.max_message_bytes,
                options.max_file_bytes,
                options.max_diagnostics_per_file,
                Arc::clone(&client.encoding),
                root_uri.clone(),
                root_name.clone(),
                diagnostic_refresh_tx,
            ))
            .await;
        let refresh_client = Arc::downgrade(&client);
        client
            .spawn_owned(async move {
                while diagnostic_refresh_rx.recv().await.is_some() {
                    let Some(client) = refresh_client.upgrade() else {
                        break;
                    };
                    client.refresh_open_diagnostics().await;
                }
            })
            .await;
        client
            .spawn_owned(stderr_loop(
                stderr,
                Arc::clone(&client.stderr),
                options.max_stderr_bytes,
            ))
            .await;

        let mut initialize_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": root_name}],
            "capabilities": {
                "general": {"positionEncodings": ["utf-8", "utf-16", "utf-32"]},
                "textDocument": {
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {"linkSupport": true},
                    "references": {},
                    "publishDiagnostics": {},
                    "diagnostic": {}
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "diagnostics": {"refreshSupport": true}
                }
            },
            "clientInfo": {"name": "clsp", "version": env!("CARGO_PKG_VERSION")}
        });
        if let Some(initialization_options) = initialization_options {
            initialize_params["initializationOptions"] = initialization_options;
        }
        let result = client
            .request_with_timeout(
                "initialize",
                initialize_params,
                initialization_timeout(options.server_id, options.request_timeout),
            )
            .await?;
        *client.encoding.write().await = PositionEncoding::from_initialize(&result);
        *client.supports_pull_diagnostics.write().await = result
            .pointer("/capabilities/diagnosticProvider")
            .is_some_and(|value| !value.is_null());
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    pub fn diagnostics_store(&self) -> &Arc<DiagnosticsStore> {
        &self.diagnostics
    }

    pub async fn sync_file(
        &self,
        path: &Path,
        language_id: &str,
    ) -> Result<Option<SyncResult>, ClspError> {
        let path = self.workspace.resolve_file(path, self.max_file_bytes)?;
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(server_error)?;
        let text = strip_utf8_bom(&text).to_owned();
        let mut documents = self.documents.lock().await;
        if documents
            .get(&path)
            .is_some_and(|document| document.text == text)
        {
            return Ok(None);
        }
        let uri = path_to_uri(&path)?;
        let (version, method, params) = match documents.get(&path) {
            Some(document) => {
                let version = document.version.saturating_add(1);
                (
                    version,
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": version},
                        "contentChanges": [{"text": text}]
                    }),
                )
            }
            None => (
                1,
                "textDocument/didOpen",
                json!({
                    "textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text}
                }),
            ),
        };
        let (baseline, baseline_available) = self.diagnostics.begin_sync(&path, version).await;
        documents.insert(
            path.clone(),
            OpenDocument {
                uri: uri.clone(),
                text,
                version,
            },
        );
        drop(documents);
        self.notify(method, params).await?;
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({"changes": [{"uri": uri, "type": if method == "textDocument/didOpen" { 1 } else { 2 }}]}),
        )
        .await?;

        if *self.supports_pull_diagnostics.read().await {
            self.pull_document_diagnostics(&path, &uri, version).await;
        }
        Ok(Some(SyncResult {
            path,
            version,
            baseline,
            baseline_available,
        }))
    }

    async fn pull_document_diagnostics(&self, path: &Path, uri: &str, version: i32) {
        if let Ok(result) = self
            .request(
                "textDocument/diagnostic",
                json!({"textDocument": {"uri": uri}}),
            )
            .await
            && let Some(items) = result.get("items").and_then(Value::as_array)
        {
            let (diagnostics, truncated) = self.convert_diagnostics(path, items).await;
            self.diagnostics
                .publish_pull_with_truncation(path.to_path_buf(), version, diagnostics, truncated)
                .await;
        }
    }

    async fn refresh_open_diagnostics(&self) {
        if !*self.supports_pull_diagnostics.read().await {
            return;
        }
        let documents = self
            .documents
            .lock()
            .await
            .iter()
            .map(|(path, document)| (path.clone(), document.uri.clone(), document.version))
            .collect::<Vec<_>>();
        for (path, uri, version) in documents {
            self.pull_document_diagnostics(&path, &uri, version).await;
        }
    }

    pub async fn close_file(&self, path: &Path) -> Result<(), ClspError> {
        let path = self.workspace.resolve_file(path, self.max_file_bytes)?;
        let document = self.documents.lock().await.remove(&path);
        if let Some(document) = document {
            self.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": document.uri}}),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn query(&self, request: QueryRequest) -> Result<QueryResult, ClspError> {
        let path = self
            .workspace
            .resolve_file(&request.path, self.max_file_bytes)?;
        let document = self
            .documents
            .lock()
            .await
            .get(&path)
            .cloned()
            .ok_or_else(|| server_error("file must be synchronized before querying"))?;
        let encoding = *self.encoding.read().await;
        let position = external_to_lsp(&document.text, request.position, encoding)?;
        let text_document = json!({"uri": document.uri});
        let (method, params) = match request.operation {
            QueryOperation::Hover => (
                "textDocument/hover",
                json!({"textDocument": text_document, "position": position}),
            ),
            QueryOperation::Definition => (
                "textDocument/definition",
                json!({"textDocument": text_document, "position": position}),
            ),
            QueryOperation::References => (
                "textDocument/references",
                json!({
                    "textDocument": text_document,
                    "position": position,
                    "context": {"includeDeclaration": request.include_declaration}
                }),
            ),
        };
        let value = self.request(method, params).await?;
        match request.operation {
            QueryOperation::Hover => Ok(QueryResult {
                hover: parse_hover(&value),
                locations: Vec::new(),
            }),
            QueryOperation::Definition | QueryOperation::References => Ok(QueryResult {
                hover: None,
                locations: self.parse_locations(&value).await,
            }),
        }
    }

    pub async fn diagnostics(&self, paths: &[PathBuf], wait: Duration) -> DiagnosticsReport {
        self.diagnostics
            .report(&self.server_id, paths, wait, self.max_diagnostics_per_file)
            .await
    }

    pub async fn shutdown(&self) -> Result<(), ClspError> {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        let result = {
            let mut child = self.child.lock().await;
            if timeout(Duration::from_secs(2), child.wait()).await.is_err() {
                child.kill().await.map_err(server_error)
            } else {
                Ok(())
            }
        };
        self.stop_owned_tasks().await;
        result
    }

    async fn spawn_owned<F>(self: &Arc<Self>, task: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::spawn(task));
    }

    async fn stop_owned_tasks(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
    }

    pub async fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lock().await.iter().cloned().collect()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub async fn is_running(&self) -> bool {
        matches!(self.child.lock().await.try_wait(), Ok(None))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, ClspError> {
        self.request_with_timeout(
            method,
            params,
            server_request_timeout(&self.server_id, self.request_timeout),
        )
        .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        request_timeout: Duration,
    ) -> Result<Value, ClspError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        if let Err(error) = self
            .write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        match timeout(request_timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(server_error("LSP response channel closed")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(server_error(format!("LSP request {method} timed out")))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), ClspError> {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    async fn write(&self, value: Value) -> Result<(), ClspError> {
        let value = normalize_outgoing_message(value);
        let mut writer = self.writer.lock().await;
        write_frame(&mut *writer, &value, self.max_message_bytes).await
    }

    async fn parse_locations(&self, value: &Value) -> Vec<Location> {
        let values: Vec<&Value> = if let Some(items) = value.as_array() {
            items.iter().collect()
        } else if value.is_object() {
            vec![value]
        } else {
            Vec::new()
        };
        let encoding = *self.encoding.read().await;
        let mut locations = Vec::new();
        for item in values.into_iter().take(MAX_LOCATIONS) {
            let uri = item
                .get("uri")
                .or_else(|| item.get("targetUri"))
                .and_then(Value::as_str);
            let range = item
                .get("range")
                .or_else(|| item.get("targetSelectionRange"));
            let (Some(uri), Some(range)) = (uri, range) else {
                continue;
            };
            let Ok(raw_path) = uri_to_path(uri) else {
                continue;
            };
            let Ok(path) = self.workspace.resolve_file(raw_path, self.max_file_bytes) else {
                continue;
            };
            let Ok(text) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            if let Ok(range) = lsp_range_to_external(&text, range, encoding) {
                let location = Location { path, range };
                if !locations.contains(&location) {
                    locations.push(location);
                }
            }
        }
        locations
    }

    async fn convert_diagnostics(&self, path: &Path, items: &[Value]) -> (Vec<Diagnostic>, bool) {
        let Ok(text) = tokio::fs::read_to_string(path).await else {
            return (Vec::new(), false);
        };
        convert_diagnostics(
            path,
            &text,
            items,
            &self.server_id,
            *self.encoding.read().await,
            self.max_diagnostics_per_file,
        )
    }
}

fn initialization_timeout(server_id: &str, request_timeout: Duration) -> Duration {
    if matches!(
        server_id,
        CLOJURE_SERVER_ID
            | ELIXIR_LS_SERVER_ID
            | JULIALS_SERVER_ID
            | KOTLIN_LS_SERVER_ID
            | RUBY_LSP_SERVER_ID
            | SOURCEKIT_LSP_SERVER_ID
    ) {
        SLOW_INITIALIZE_TIMEOUT
    } else {
        request_timeout
    }
}

fn server_request_timeout(server_id: &str, request_timeout: Duration) -> Duration {
    if matches!(server_id, JDTLS_SERVER_ID | JULIALS_SERVER_ID) {
        request_timeout.max(SLOW_REQUEST_TIMEOUT)
    } else {
        request_timeout
    }
}

#[allow(clippy::too_many_arguments)]
async fn reader_loop(
    mut stdout: tokio::process::ChildStdout,
    writer: Arc<Mutex<ChildStdin>>,
    pending: PendingRequests,
    documents: Arc<Mutex<HashMap<PathBuf, OpenDocument>>>,
    diagnostics: Arc<DiagnosticsStore>,
    workspace: Workspace,
    server_id: String,
    server_root: PathBuf,
    max_message_bytes: usize,
    max_file_bytes: u64,
    max_diagnostics_per_file: usize,
    encoding: Arc<RwLock<PositionEncoding>>,
    root_uri: String,
    root_name: String,
    diagnostic_refresh: mpsc::Sender<()>,
) {
    loop {
        let message = match read_frame(&mut stdout, max_message_bytes).await {
            Ok(message) => message,
            Err(error) => {
                fail_pending(&pending, error).await;
                return;
            }
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64)
            && message.get("method").is_none()
        {
            if let Some(sender) = pending.lock().await.remove(&id) {
                let result = if let Some(error) = message.get("error") {
                    Err(server_error(
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("LSP error"),
                    ))
                } else {
                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = sender.send(result);
            }
            continue;
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        if let Some(id) = message.get("id").cloned() {
            let refresh_diagnostics = method == "workspace/diagnostic/refresh";
            let response = server_request_response(
                &server_id,
                method,
                message.get("params"),
                &root_uri,
                &root_name,
            );
            let value = match response {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, text)) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}})
                }
            };
            let _ = write_frame(&mut *writer.lock().await, &value, max_message_bytes).await;
            if refresh_diagnostics {
                let _ = diagnostic_refresh.try_send(());
            }
            continue;
        }
        if let Some(response) =
            server_notification_response(&server_id, method, message.get("params"), &server_root)
        {
            let _ = write_frame(&mut *writer.lock().await, &response, max_message_bytes).await;
            continue;
        }
        if method != "textDocument/publishDiagnostics" {
            continue;
        }
        let Some(params) = message.get("params") else {
            continue;
        };
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            continue;
        };
        let Ok(raw_path) = uri_to_path(uri) else {
            continue;
        };
        let Ok(path) = workspace.resolve_file(raw_path, max_file_bytes) else {
            continue;
        };
        let open_document = {
            let open = documents.lock().await;
            open.get(&path)
                .map(|document| (document.text.clone(), document.version))
        };
        let (text, open_version) = match open_document {
            Some((text, version)) => (text, Some(version)),
            None => match tokio::fs::read_to_string(&path).await {
                Ok(text) => (strip_utf8_bom(&text).to_owned(), None),
                Err(_) => continue,
            },
        };
        let items = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (converted, truncated) = convert_diagnostics(
            &path,
            &text,
            items,
            &server_id,
            *encoding.read().await,
            max_diagnostics_per_file,
        );
        let reported_version = params
            .get("version")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let version = diagnostic_version(&server_id, reported_version, open_version);
        diagnostics
            .publish_with_truncation(path, version, converted, truncated)
            .await;
    }
}

async fn stderr_loop(
    stderr: tokio::process::ChildStderr,
    lines: Arc<Mutex<VecDeque<String>>>,
    max_bytes: usize,
) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let mut queue = lines.lock().await;
        queue.push_back(line.trim_end().chars().take(2_048).collect());
        while queue.iter().map(String::len).sum::<usize>() > max_bytes {
            queue.pop_front();
        }
        line.clear();
    }
}

async fn fail_pending(pending: &PendingRequests, error: ClspError) {
    for (_, sender) in pending.lock().await.drain() {
        let _ = sender.send(Err(error.clone()));
    }
}

fn server_notification_response(
    server_id: &str,
    method: &str,
    params: Option<&Value>,
    server_root: &Path,
) -> Option<Value> {
    if server_id != VUE_SERVER_ID || method != "tsserver/request" {
        return None;
    }
    let request = params?.as_array()?.first()?.as_array()?;
    let request_id = request.first()?.as_u64()?;
    let command = request.get(1)?.as_str()?;
    let body = if command == "_vue:projectInfo" {
        let config = ["tsconfig.json", "jsconfig.json"]
            .into_iter()
            .map(|name| server_root.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| server_root.join("tsconfig.json"));
        json!({
            "configFileName": child_process_path(&config)
                .to_string_lossy()
                .replace('\\', "/")
        })
    } else {
        Value::Null
    };
    Some(json!({
        "jsonrpc": "2.0",
        "method": "tsserver/response",
        "params": [[request_id, body]]
    }))
}

fn server_request_response(
    server_id: &str,
    method: &str,
    params: Option<&Value>,
    root_uri: &str,
    root_name: &str,
) -> Result<Value, (i64, &'static str)> {
    match method {
        "workspace/configuration" => {
            let count = params
                .and_then(|value| value.get("items"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            if server_id == ESLINT_SERVER_ID {
                Ok(Value::Array(vec![
                    json!({
                        "validate": "on",
                        "workspaceFolder": {"uri": root_uri, "name": root_name}
                    });
                    count
                ]))
            } else if server_id == PRISMA_SERVER_ID {
                Ok(Value::Array(vec![json!({}); count]))
            } else {
                Ok(Value::Array(vec![Value::Null; count]))
            }
        }
        "eslint/noConfig" | "eslint/noLibrary" | "eslint/openDoc" | "eslint/probeFailed"
            if server_id == ESLINT_SERVER_ID =>
        {
            Ok(Value::Null)
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "window/showMessageRequest"
        | "workspace/diagnostic/refresh" => Ok(Value::Null),
        "workspace/workspaceFolders" => Ok(json!([{"uri": root_uri, "name": root_name}])),
        "workspace/applyEdit" => Ok(json!({
            "applied": false,
            "failureReason": "CLSP never applies WorkspaceEdit"
        })),
        _ => Err((-32601, "method not supported by CLSP")),
    }
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_body_bytes: usize,
) -> Result<Value, ClspError> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= HEADER_LIMIT {
            return Err(server_error("LSP header exceeds limit"));
        }
        let byte = reader.read_u8().await.map_err(server_error)?;
        header.push(byte);
    }
    let text = std::str::from_utf8(&header).map_err(server_error)?;
    let mut content_length = None;
    for line in text.split("\r\n").filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(server_error("malformed LSP header"));
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(server_error("duplicate LSP Content-Length"));
            }
            content_length = Some(value.trim().parse::<usize>().map_err(server_error)?);
        }
    }
    let length = content_length.ok_or_else(|| server_error("missing LSP Content-Length"))?;
    if length > max_body_bytes {
        return Err(server_error("LSP body exceeds configured limit"));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.map_err(server_error)?;
    serde_json::from_slice(&body).map_err(server_error)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
    max_body_bytes: usize,
) -> Result<(), ClspError> {
    let body = serde_json::to_vec(value).map_err(server_error)?;
    if body.len() > max_body_bytes {
        return Err(server_error("LSP response exceeds configured limit"));
    }
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .map_err(server_error)?;
    writer.write_all(&body).await.map_err(server_error)?;
    writer.flush().await.map_err(server_error)
}

pub fn external_to_lsp(
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Result<Value, ClspError> {
    if position.line == 0 || position.column == 0 {
        return Err(server_error("external positions are one-based"));
    }
    let line = text
        .split('\n')
        .nth((position.line - 1) as usize)
        .ok_or_else(|| server_error("line is outside the document"))?
        .trim_end_matches('\r');
    let scalar_index = (position.column - 1) as usize;
    let prefix: String = line.chars().take(scalar_index).collect();
    if prefix.chars().count() != scalar_index {
        return Err(server_error("column is outside the line"));
    }
    let character = encoded_units(&prefix, encoding);
    Ok(json!({"line": position.line - 1, "character": character}))
}

pub fn lsp_to_external(
    text: &str,
    line: u32,
    character: u32,
    encoding: PositionEncoding,
) -> Result<Position, ClspError> {
    let line_text = text
        .split('\n')
        .nth(line as usize)
        .ok_or_else(|| server_error("LSP line is outside the document"))?
        .trim_end_matches('\r');
    let mut units = 0u32;
    let mut scalars = 0u32;
    for value in line_text.chars() {
        if units == character {
            break;
        }
        units = units.saturating_add(match encoding {
            PositionEncoding::Utf8 => value.len_utf8() as u32,
            PositionEncoding::Utf16 => value.len_utf16() as u32,
            PositionEncoding::Utf32 => 1,
        });
        scalars = scalars.saturating_add(1);
        if units > character {
            return Err(server_error("LSP character splits an encoded scalar"));
        }
    }
    if units != character {
        return Err(server_error("LSP character is outside the line"));
    }
    Ok(Position {
        line: line + 1,
        column: scalars + 1,
    })
}

fn encoded_units(text: &str, encoding: PositionEncoding) -> u32 {
    match encoding {
        PositionEncoding::Utf8 => text.len() as u32,
        PositionEncoding::Utf16 => text.encode_utf16().count() as u32,
        PositionEncoding::Utf32 => text.chars().count() as u32,
    }
}

fn lsp_range_to_external(
    text: &str,
    range: &Value,
    encoding: PositionEncoding,
) -> Result<TextRange, ClspError> {
    let convert = |name: &str| -> Result<Position, ClspError> {
        let value = range
            .get(name)
            .ok_or_else(|| server_error("diagnostic range is missing"))?;
        let line = value
            .get("line")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| server_error("invalid LSP line"))?;
        let character = value
            .get("character")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| server_error("invalid LSP character"))?;
        lsp_to_external(text, line, character, encoding)
    };
    Ok(TextRange {
        start: convert("start")?,
        end: convert("end")?,
    })
}

fn convert_diagnostics(
    path: &Path,
    text: &str,
    items: &[Value],
    server_id: &str,
    encoding: PositionEncoding,
    max_items: usize,
) -> (Vec<Diagnostic>, bool) {
    let truncated = items.len() > max_items;
    let diagnostics = items
        .iter()
        .take(max_items)
        .filter_map(|item| {
            let range = lsp_range_to_external(text, item.get("range")?, encoding).ok()?;
            let severity = match item.get("severity").and_then(Value::as_u64) {
                Some(1) => DiagnosticSeverity::Error,
                Some(2) => DiagnosticSeverity::Warning,
                Some(3) => DiagnosticSeverity::Information,
                _ => DiagnosticSeverity::Hint,
            };
            let code = item.get("code").and_then(|code| match code {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                _ => None,
            });
            Some(Diagnostic {
                path: path.to_path_buf(),
                range,
                severity,
                code,
                source: item
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: item.get("message")?.as_str()?.chars().take(4_096).collect(),
                server_id: server_id.into(),
            })
        })
        .collect();
    (diagnostics, truncated)
}

fn dedupe_diagnostics(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut seen = BTreeSet::new();
    diagnostics
        .into_iter()
        .filter(|diagnostic| seen.insert(edit_diagnostic_key(diagnostic)))
        .collect()
}

fn parse_hover(value: &Value) -> Option<String> {
    let contents = value.get("contents")?;
    fn text(value: &Value, output: &mut Vec<String>) {
        match value {
            Value::String(value) => output.push(value.clone()),
            Value::Array(values) => values.iter().for_each(|value| text(value, output)),
            Value::Object(values) => {
                if let Some(value) = values.get("value").and_then(Value::as_str) {
                    output.push(value.into());
                }
            }
            _ => {}
        }
    }
    let mut output = Vec::new();
    text(contents, &mut output);
    (!output.is_empty()).then(|| output.join("\n\n").chars().take(MAX_HOVER_CHARS).collect())
}

fn path_to_uri(path: &Path) -> Result<String, ClspError> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| server_error("cannot convert path to file URI"))
}

fn uri_to_path(uri: &str) -> Result<PathBuf, ClspError> {
    Url::parse(uri)
        .map_err(server_error)?
        .to_file_path()
        .map_err(|_| server_error("LSP URI is not a local file"))
}

fn server_initialization_options(
    server_id: &str,
    server_root: &Path,
    workspace_root: &Path,
    executable: &Path,
    npm_modules_root: Option<&Path>,
) -> Result<Option<Value>, ClspError> {
    if server_id == DENO_SERVER_ID {
        return Ok(Some(json!({"enable": true})));
    }
    if server_id == FSHARP_SERVER_ID {
        return Ok(Some(json!({"AutomaticWorkspaceInit": true})));
    }
    if server_id == INTELEPHENSE_SERVER_ID {
        return Ok(Some(json!({"telemetry": {"enabled": false}})));
    }
    if server_id == JDTLS_SERVER_ID {
        return Ok(Some(json!({
            "workspaceFolders": [path_to_uri(server_root)?],
            "settings": {}
        })));
    }
    if server_id == TERRAFORM_SERVER_ID {
        return Ok(Some(json!({
            "experimentalFeatures": {
                "prefillRequiredFields": true,
                "validateOnSave": true
            }
        })));
    }
    if server_id == YAML_LS_SERVER_ID && uses_node_host(server_id, executable) {
        let l10n = child_process_path(&yaml_extension_l10n(executable)?);
        return Ok(Some(json!({"l10nPath": l10n.to_string_lossy()})));
    }
    if !matches!(server_id, ASTRO_SERVER_ID | TYPESCRIPT_SERVER_ID) {
        return Ok(None);
    }
    let tsdk = typescript_sdk(server_root, workspace_root, executable, npm_modules_root)
        .ok_or_else(|| {
            runtime_error(format!(
                "{server_id} language server requires typescript/lib/tsserver.js"
            ))
        })?;
    let tsdk = child_process_path(&tsdk);
    Ok(Some(if server_id == ASTRO_SERVER_ID {
        json!({
            "typescript": {"tsdk": tsdk.to_string_lossy()}
        })
    } else {
        json!({
            "tsserver": {"path": tsdk.join("tsserver.js").to_string_lossy()}
        })
    }))
}

fn server_runtime_args(
    server_id: &str,
    server_root: &Path,
    workspace_root: &Path,
    executable: &Path,
    npm_modules_root: Option<&Path>,
) -> Result<Vec<String>, ClspError> {
    if server_id != VUE_SERVER_ID {
        return Ok(Vec::new());
    }
    let tsdk = typescript_sdk(server_root, workspace_root, executable, npm_modules_root)
        .ok_or_else(|| runtime_error("vue language server requires typescript/lib/tsserver.js"))?;
    Ok(vec![format!(
        "--tsdk={}",
        child_process_path(&tsdk).to_string_lossy()
    )])
}

fn uses_dotnet_host(server_id: &str, executable: &Path) -> bool {
    server_id == FSHARP_SERVER_ID
        && executable
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
}

fn uses_node_host(server_id: &str, executable: &Path) -> bool {
    server_id == ESLINT_SERVER_ID
        || (matches!(
            server_id,
            INTELEPHENSE_SERVER_ID
                | PRISMA_SERVER_ID
                | PYRIGHT_SERVER_ID
                | SVELTE_SERVER_ID
                | VUE_SERVER_ID
                | YAML_LS_SERVER_ID
        ) && executable
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("js")))
}

fn uses_jdtls_java_host(server_id: &str, executable: &Path) -> bool {
    server_id == JDTLS_SERVER_ID
        && executable
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
}

fn julials_extension_command(project: &Path) -> Result<Command, ClspError> {
    let environment = julials_extension_environment(project)?;
    let julia = which::which("julia")
        .map_err(|_| runtime_error("Julia extension LanguageServer requires Julia on PATH"))?;
    Ok(julials_command(&julia, &environment))
}

fn julials_command(julia: &Path, environment: &Path) -> Command {
    let mut command = Command::new(julia);
    command.arg(format!("--project={}", environment.to_string_lossy()));
    command
}

async fn jdtls_java_command(
    launcher: &Path,
    root: &Path,
    probe_timeout: Duration,
) -> Result<Command, ClspError> {
    let layout = jdtls_extension_layout(launcher)?;
    let (java, major) = jdtls_java_for_launcher(launcher, root, probe_timeout).await?;
    let mut command = Command::new(java);
    command.args(jdtls_vm_args(&layout.configuration, launcher, major));
    Ok(command)
}

fn jdtls_vm_args(configuration: &Path, launcher: &Path, java_major: u64) -> Vec<String> {
    let mut args = Vec::new();
    if java_major >= 24 {
        args.extend([
            "-Djdk.xml.maxGeneralEntitySizeLimit=0".into(),
            "-Djdk.xml.totalEntitySizeLimit=0".into(),
        ]);
    }
    args.extend([
        "-Declipse.application=org.eclipse.jdt.ls.core.id1".into(),
        "-Dosgi.bundles.defaultStartLevel=4".into(),
        "-Declipse.product=org.eclipse.jdt.ls.core.product".into(),
        "-Dosgi.checkConfiguration=true".into(),
        format!(
            "-Dosgi.sharedConfiguration.area={}",
            configuration.to_string_lossy()
        ),
        "-Dosgi.sharedConfiguration.area.readOnly=true".into(),
        "-Dosgi.configuration.cascaded=true".into(),
        "-Xms1G".into(),
        "--add-modules=ALL-SYSTEM".into(),
        "--add-opens".into(),
        "java.base/java.util=ALL-UNNAMED".into(),
        "--add-opens".into(),
        "java.base/java.lang=ALL-UNNAMED".into(),
        "-jar".into(),
        launcher.to_string_lossy().into_owned(),
    ]);
    args
}

fn strip_utf8_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn diagnostic_version(
    server_id: &str,
    reported_version: Option<i32>,
    open_version: Option<i32>,
) -> Option<i32> {
    reported_version.or_else(|| {
        (matches!(server_id, FSHARP_SERVER_ID | PRISMA_SERVER_ID))
            .then_some(open_version)
            .flatten()
    })
}

fn typescript_sdk(
    server_root: &Path,
    workspace_root: &Path,
    executable: &Path,
    npm_modules_root: Option<&Path>,
) -> Option<PathBuf> {
    if server_root.starts_with(workspace_root) {
        for ancestor in server_root.ancestors() {
            if !ancestor.starts_with(workspace_root) {
                break;
            }
            if let Some(tsdk) = typescript_sdk_in(&ancestor.join("node_modules")) {
                return Some(tsdk);
            }
            if ancestor == workspace_root {
                break;
            }
        }
    }
    if let Some(tsdk) = npm_modules_root.and_then(typescript_sdk_in) {
        return Some(tsdk);
    }
    executable
        .ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name == "node_modules")
        })
        .and_then(typescript_sdk_in)
}

fn normalize_outgoing_message(mut value: Value) -> Value {
    if value.get("params").is_some_and(Value::is_null) {
        value
            .as_object_mut()
            .expect("JSON-RPC messages are objects")
            .remove("params");
    }
    value
}

fn typescript_sdk_in(node_modules: &Path) -> Option<PathBuf> {
    let tsdk = node_modules.join("typescript").join("lib");
    tsdk.join("tsserver.js").is_file().then_some(tsdk)
}

fn server_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::ServerUnavailable, error.to_string()).retryable()
}

fn runtime_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::RuntimeUnavailable, error.to_string()).retryable()
}

#[cfg(test)]
#[path = "../tests/unit/lsp.rs"]
mod tests;

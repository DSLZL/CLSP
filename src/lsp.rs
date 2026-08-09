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
    sync::{Mutex, Notify, RwLock, oneshot},
    time::timeout,
};
use url::Url;

use crate::{
    installer::sanitize_command,
    protocol::{
        ClspError, Diagnostic, DiagnosticSeverity, DiagnosticsReport, ErrorCode, Location,
        Position, QueryOperation, QueryRequest, QueryResult, SourceFreshness, TextRange,
    },
    workspace::Workspace,
};

const HEADER_LIMIT: usize = 8 * 1024;
const MAX_HOVER_CHARS: usize = 64 * 1024;
const MAX_LOCATIONS: usize = 100;
const ASTRO_SERVER_ID: &str = "astro";
const CLOJURE_SERVER_ID: &str = "clojure-lsp";
const ELIXIR_LS_SERVER_ID: &str = "elixir-ls";
const DENO_SERVER_ID: &str = "deno";
const TYPESCRIPT_SERVER_ID: &str = "typescript";
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
        let baseline_available = records.contains_key(path);
        let record = records.entry(path.to_path_buf()).or_default();
        let baseline = record.diagnostics.iter().map(diagnostic_key).collect();
        record.synchronized_version = Some(version);
        record.fresh = false;
        record.reason = Some("awaiting_diagnostics".into());
        (baseline, baseline_available)
    }

    pub async fn publish(&self, path: PathBuf, version: Option<i32>, diagnostics: Vec<Diagnostic>) {
        let mut records = self.records.lock().await;
        let record = records.entry(path).or_default();
        record.push_generation = record.push_generation.saturating_add(1);
        let expected = record.synchronized_version;
        match (version, expected) {
            (Some(actual), Some(expected)) if actual == expected => {
                record.diagnostics = dedupe_diagnostics(diagnostics);
                record.received_version = Some(actual);
                record.fresh = true;
                record.reason = None;
            }
            (Some(actual), Some(expected)) if actual > expected => {
                record.fresh = false;
                record.reason = Some("future_document_version".into());
            }
            (Some(actual), _) => {
                record.diagnostics = dedupe_diagnostics(diagnostics);
                record.received_version = Some(actual);
                record.fresh = false;
                record.reason = Some("stale_document_version".into());
            }
            (None, _) => {
                record.diagnostics = dedupe_diagnostics(diagnostics);
                record.received_version = None;
                record.fresh = false;
                record.reason = Some("diagnostic_version_unavailable".into());
            }
        }
        drop(records);
        self.changed.notify_waiters();
    }

    pub async fn publish_pull(&self, path: PathBuf, version: i32, diagnostics: Vec<Diagnostic>) {
        let mut records = self.records.lock().await;
        let record = records.entry(path).or_default();
        if record.synchronized_version == Some(version) {
            record.diagnostics = dedupe_diagnostics(diagnostics);
            record.received_version = Some(version);
            record.fresh = true;
            record.reason = None;
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
        records
            .get(&sync.path)
            .into_iter()
            .flat_map(|record| record.diagnostics.iter())
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .filter(|diagnostic| !sync.baseline.contains(&diagnostic_key(diagnostic)))
            .cloned()
            .collect()
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
                fresh: record.fresh,
                reason: record.reason.clone(),
                document_version: record.received_version,
            });
        }
        let fresh = !sources.is_empty() && sources.iter().all(|source| source.fresh);
        DiagnosticsReport {
            diagnostics,
            fresh,
            sources,
            baseline_available: paths.iter().all(|path| records.contains_key(path)),
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
        let mut command = Command::new(options.executable);
        command.args(options.args).current_dir(options.root);
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
        });

        tokio::spawn(reader_loop(
            stdout,
            Arc::clone(&client.writer),
            Arc::clone(&client.pending),
            Arc::clone(&client.documents),
            Arc::clone(&client.diagnostics),
            client.workspace.clone(),
            client.server_id.clone(),
            options.max_message_bytes,
            options.max_file_bytes,
            options.max_diagnostics_per_file,
            Arc::clone(&client.encoding),
            root_uri.clone(),
            root_name.clone(),
        ));
        tokio::spawn(stderr_loop(
            stderr,
            Arc::clone(&client.stderr),
            options.max_stderr_bytes,
        ));

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
                    "diagnostic": {}
                },
                "workspace": {"workspaceFolders": true, "configuration": true}
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

        if *self.supports_pull_diagnostics.read().await
            && let Ok(result) = self
                .request(
                    "textDocument/diagnostic",
                    json!({"textDocument": {"uri": uri}}),
                )
                .await
            && let Some(items) = result.get("items").and_then(Value::as_array)
        {
            let diagnostics = self.convert_diagnostics(&path, items).await;
            self.diagnostics
                .publish_pull(path.clone(), version, diagnostics)
                .await;
        }
        Ok(Some(SyncResult {
            path,
            version,
            baseline,
            baseline_available,
        }))
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
        let mut child = self.child.lock().await;
        if timeout(Duration::from_secs(2), child.wait()).await.is_err() {
            child.kill().await.map_err(server_error)?;
        }
        Ok(())
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
        self.request_with_timeout(method, params, self.request_timeout)
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

    async fn convert_diagnostics(&self, path: &Path, items: &[Value]) -> Vec<Diagnostic> {
        let Ok(text) = tokio::fs::read_to_string(path).await else {
            return Vec::new();
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
    if matches!(server_id, CLOJURE_SERVER_ID | ELIXIR_LS_SERVER_ID) {
        SLOW_INITIALIZE_TIMEOUT
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
    max_message_bytes: usize,
    max_file_bytes: u64,
    max_diagnostics_per_file: usize,
    encoding: Arc<RwLock<PositionEncoding>>,
    root_uri: String,
    root_name: String,
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
            let response =
                server_request_response(method, message.get("params"), &root_uri, &root_name);
            let value = match response {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, text)) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}})
                }
            };
            let _ = write_frame(&mut *writer.lock().await, &value, max_message_bytes).await;
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
        let text = {
            let open = documents.lock().await;
            open.get(&path).map(|document| document.text.clone())
        };
        let text = match text {
            Some(text) => text,
            None => match tokio::fs::read_to_string(&path).await {
                Ok(text) => text,
                Err(_) => continue,
            },
        };
        let items = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let converted = convert_diagnostics(
            &path,
            &text,
            items,
            &server_id,
            *encoding.read().await,
            max_diagnostics_per_file,
        );
        let version = params
            .get("version")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        diagnostics.publish(path, version, converted).await;
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

fn server_request_response(
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
            Ok(Value::Array(vec![Value::Null; count]))
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "window/showMessageRequest" => Ok(Value::Null),
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
) -> Vec<Diagnostic> {
    items
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
        .collect()
}

fn dedupe_diagnostics(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut seen = BTreeSet::new();
    diagnostics
        .into_iter()
        .filter(|diagnostic| seen.insert(diagnostic_key(diagnostic)))
        .collect()
}

fn diagnostic_key(diagnostic: &Diagnostic) -> String {
    format!(
        "{}:{}:{}:{}:{}:{:?}:{}:{}:{}",
        diagnostic.path.display(),
        diagnostic.range.start.line,
        diagnostic.range.start.column,
        diagnostic.range.end.line,
        diagnostic.range.end.column,
        diagnostic.severity,
        diagnostic.code.as_deref().unwrap_or_default(),
        diagnostic.source.as_deref().unwrap_or_default(),
        diagnostic.message
    )
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
    if !matches!(server_id, ASTRO_SERVER_ID | TYPESCRIPT_SERVER_ID) {
        return Ok(None);
    }
    let tsdk = astro_typescript_sdk(server_root, workspace_root, executable, npm_modules_root)
        .ok_or_else(|| {
            runtime_error(format!(
                "{server_id} language server requires typescript/lib/tsserver.js"
            ))
        })?;
    Ok((server_id == ASTRO_SERVER_ID).then(|| {
        json!({
            "typescript": {"tsdk": tsdk.to_string_lossy()}
        })
    }))
}

fn astro_typescript_sdk(
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
mod tests {
    use super::*;

    fn write_typescript_sdk(root: &Path) -> PathBuf {
        let tsdk = root.join("node_modules").join("typescript").join("lib");
        std::fs::create_dir_all(&tsdk).unwrap();
        std::fs::write(tsdk.join("tsserver.js"), "").unwrap();
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

    #[test]
    fn astro_initialization_prefers_nearest_project_typescript() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let root = workspace.join("packages").join("site");
        std::fs::create_dir_all(&root).unwrap();
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
    }

    #[test]
    fn astro_initialization_uses_manager_typescript() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let root = workspace.join("site");
        std::fs::create_dir_all(&root).unwrap();
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

        assert!(
            server_initialization_options(
                TYPESCRIPT_SERVER_ID,
                &root,
                &workspace,
                &manager.join("bin/typescript-language-server.cmd"),
                Some(&manager.join("node_modules")),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn only_astro_requires_typescript_initialization() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let executable = root.join("astro-ls.cmd");
        assert!(
            server_initialization_options("rust", root, root, &executable, None)
                .unwrap()
                .is_none()
        );
        let error = server_initialization_options(ASTRO_SERVER_ID, root, root, &executable, None)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RuntimeUnavailable);
        assert!(
            error
                .message
                .contains("requires typescript/lib/tsserver.js")
        );
    }

    #[test]
    fn deno_initialization_enables_the_server() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let options =
            server_initialization_options(DENO_SERVER_ID, root, root, &root.join("deno.exe"), None)
                .unwrap()
                .unwrap();
        assert_eq!(options, json!({"enable": true}));
    }

    #[test]
    fn only_slow_starting_servers_get_the_long_initialize_timeout() {
        let normal = Duration::from_secs(10);
        for server_id in [CLOJURE_SERVER_ID, ELIXIR_LS_SERVER_ID] {
            assert_eq!(
                initialization_timeout(server_id, normal),
                Duration::from_secs(300)
            );
        }
        assert_eq!(initialization_timeout("rust", normal), normal);
    }
}

use std::{collections::BTreeMap, path::PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "clsp-rpc/2";
pub const IDE_SESSION_ID_HEX_LEN: usize = 64;
pub const IDE_ACTION_QUEUE_CAPACITY: usize = 16;
pub const IDE_STDIO_MAX_BYTES: usize = 256 * 1024;
pub const IDE_SELECTION_MAX_BYTES: usize = 8 * 1024;
pub const IDE_HOOK_CONTEXT_MAX_BYTES: usize = 12 * 1024;
pub const IDE_DIAGNOSTICS_MAX_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidConfig,
    InvalidRequest,
    PathOutsideWorkspace,
    UnsupportedFile,
    RuntimeUnavailable,
    ArtifactUnavailable,
    IntegrityFailure,
    ServerUnavailable,
    BrokerUnavailable,
    AuthenticationFailed,
    ProtocolMismatch,
    DiagnosticsTimeout,
    DiagnosticsStale,
    IdeUnavailable,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, thiserror::Error, PartialEq, Eq)]
#[error("{code:?}: {message}")]
pub struct ClspError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl ClspError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            server_id: None,
            path: None,
        }
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn for_server(mut self, server_id: impl Into<String>) -> Self {
        self.server_id = Some(server_id.into());
        self
    }

    pub fn for_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientKey {
    pub root: PathBuf,
    pub server_id: String,
    pub artifact_version: String,
    pub config_digest: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    Detected,
    Resolving,
    Installing,
    Starting,
    Running,
    Stopping,
    Stopped,
    Blocked,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct Position {
    /// One-based line number.
    pub line: u32,
    /// One-based Unicode scalar column.
    pub column: u32,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct TextRange {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub range: TextRange,
    pub severity: DiagnosticSeverity,
    pub code: Option<String>,
    pub source: Option<String>,
    pub message: String,
    pub server_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct SourceFreshness {
    pub server_id: String,
    pub fresh: bool,
    pub reason: Option<String>,
    pub document_version: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DiagnosticsReport {
    pub diagnostics: Vec<Diagnostic>,
    pub fresh: bool,
    pub sources: Vec<SourceFreshness>,
    pub baseline_available: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ServerSnapshot {
    pub key: ClientKey,
    pub state: ServerState,
    pub executable: Option<PathBuf>,
    pub pid: Option<u32>,
    pub detail: Option<String>,
    pub install_progress: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct BrokerSnapshot {
    pub protocol: String,
    pub workspace: PathBuf,
    pub broker_pid: u32,
    pub sequence: u64,
    pub servers: Vec<ServerSnapshot>,
    pub active_connections: usize,
    pub active_leases: usize,
    #[serde(default)]
    pub active_ide_sessions: usize,
    pub hook_last_seen_ms: Option<u64>,
    pub hook_same_turn_ready: bool,
    pub recent_events: Vec<BrokerEvent>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct BrokerEvent {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub body: EventBody,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    ServerState {
        key: ClientKey,
        state: ServerState,
        detail: Option<String>,
    },
    InstallProgress {
        server_id: String,
        progress: f32,
    },
    DiagnosticsChanged {
        path: PathBuf,
        server_id: String,
    },
    LeaseChanged {
        session_id: String,
        active: bool,
    },
    BrokerMessage {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Mcp,
    Hook,
    Ide,
    Status,
    Tui,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(deny_unknown_fields)]
pub struct IdePosition {
    /// Zero-based UTF-16 line number.
    pub line: u32,
    /// Zero-based UTF-16 character offset.
    pub character: u32,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(deny_unknown_fields)]
pub struct IdeRange {
    pub start: IdePosition,
    pub end: IdePosition,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdeSelection {
    pub start: IdePosition,
    pub end: IdePosition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_omitted: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdeEditorContext {
    pub active_file: PathBuf,
    pub document_version: u32,
    pub dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<IdeSelection>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdeDiagnostic {
    pub path: PathBuf,
    pub range: IdeRange,
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdeDiagnosticsReport {
    pub source: String,
    pub fresh: bool,
    pub position_encoding: String,
    pub diagnostics: Vec<IdeDiagnostic>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    Add,
    Update,
    Delete,
    Move,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditTarget {
    pub kind: EditKind,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_to: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdeDiffPair {
    pub left: PathBuf,
    pub right: PathBuf,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdeAction {
    GetEditorContext {},
    GetDiagnostics {
        file: Option<PathBuf>,
        minimum_severity: DiagnosticSeverity,
    },
    PrepareEdit {
        targets: Vec<PathBuf>,
    },
    OpenDiff {
        pairs: Vec<IdeDiffPair>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdePrepareOutcome {
    Ready,
    Cancelled,
    SaveFailed,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdeActionResult {
    EditorContext {
        context: Option<IdeEditorContext>,
    },
    Diagnostics {
        items: Vec<IdeDiagnostic>,
        truncated: bool,
    },
    Prepared {
        outcome: IdePrepareOutcome,
        message: Option<String>,
    },
    DiffOpened {
        opened: usize,
        failed: usize,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdeActionEnvelope {
    pub action_id: u64,
    pub action: IdeAction,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdeHostInput {
    ActionResult {
        action_id: u64,
        result: IdeActionResult,
    },
    Shutdown {},
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdeHostOutput {
    Status { state: String },
    Action(IdeActionEnvelope),
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdeCandidate {
    pub session_id: String,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Hello {
    pub protocol: String,
    pub token: String,
    pub client_kind: ClientKind,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RequestEnvelope {
    pub id: u64,
    pub body: RpcRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ResponseEnvelope {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<RpcResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ClspError>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct EventEnvelope {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub body: EventBody,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireMessage {
    Hello(Hello),
    Request(RequestEnvelope),
    Response(ResponseEnvelope),
    Event(EventEnvelope),
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryOperation {
    Hover,
    Definition,
    References,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct QueryRequest {
    pub operation: QueryOperation,
    pub path: PathBuf,
    pub position: Position,
    #[serde(default)]
    pub include_declaration: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub range: TextRange,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct QueryResult {
    pub hover: Option<String>,
    pub locations: Vec<Location>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RpcRequest {
    AcquireLease {
        session_id: String,
    },
    RenewLease {
        session_id: String,
    },
    ReleaseLease {
        session_id: String,
    },
    Discover,
    EnsureFile {
        path: PathBuf,
    },
    Query(QueryRequest),
    Diagnostics {
        paths: Vec<PathBuf>,
        minimum_severity: Option<DiagnosticSeverity>,
        wait_ms: Option<u64>,
    },
    Snapshot,
    Subscribe {
        after_seq: u64,
    },
    RetryServer {
        key: ClientKey,
    },
    StartServer {
        key: ClientKey,
    },
    StopServer {
        key: ClientKey,
    },
    SyncFiles {
        paths: Vec<PathBuf>,
    },
    SyncIdeDiagnostics {
        session_id: String,
        codex_session_id: String,
        tool_use_id: String,
        paths: Vec<PathBuf>,
    },
    RegisterIde {
        session_id: String,
        adapter_version: String,
        workspace_root: PathBuf,
    },
    UnregisterIde {
        session_id: String,
    },
    PollIdeActions {
        session_id: String,
        wait_ms: u64,
    },
    CompleteIdeAction {
        session_id: String,
        action_id: u64,
        result: IdeActionResult,
    },
    ListIdeCandidates {
        cwd: PathBuf,
    },
    GetIdeContext {
        session_id: String,
    },
    GetIdeDiagnostics {
        session_id: String,
        file: Option<PathBuf>,
        minimum_severity: Option<DiagnosticSeverity>,
    },
    PrepareEdit {
        session_id: String,
        codex_session_id: String,
        tool_use_id: String,
        targets: Vec<EditTarget>,
    },
    OpenEditReview {
        codex_session_id: String,
        tool_use_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcResponse {
    Ack,
    Snapshot(BrokerSnapshot),
    Query(QueryResult),
    Diagnostics(DiagnosticsReport),
    Sync {
        paths: Vec<PathBuf>,
        new_errors: Vec<Diagnostic>,
        fresh: bool,
        baseline_available: bool,
    },
    Events {
        events: Vec<BrokerEvent>,
    },
    Paths {
        paths: Vec<PathBuf>,
    },
    Data {
        values: BTreeMap<String, String>,
    },
    IdeCandidates {
        candidates: Vec<IdeCandidate>,
    },
    IdeAction {
        action: Option<IdeActionEnvelope>,
    },
    IdeContext {
        context: Option<IdeEditorContext>,
    },
    IdeDiagnostics(IdeDiagnosticsReport),
    IdePrepared {
        review_available: bool,
        partial: bool,
    },
    IdeReview {
        opened: usize,
        partial: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_round_trip_preserves_tagged_request() {
        let message = WireMessage::Request(RequestEnvelope {
            id: 7,
            body: RpcRequest::AcquireLease {
                session_id: "session-1".into(),
            },
        });
        let encoded = serde_json::to_vec(&message).unwrap();
        let decoded: WireMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn protocol_round_trip_preserves_ide_diagnostic_sync() {
        let request = RpcRequest::SyncIdeDiagnostics {
            session_id: "a".repeat(IDE_SESSION_ID_HEX_LEN),
            codex_session_id: "codex-session".into(),
            tool_use_id: "tool-call".into(),
            paths: vec![PathBuf::from("C:/workspace/src/lib.rs")],
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            serde_json::from_slice::<RpcRequest>(&encoded).unwrap(),
            request
        );
    }

    #[test]
    fn ide_stdio_rejects_unknown_fields() {
        let input = br#"{"type":"shutdown","extra":true}"#;
        assert!(serde_json::from_slice::<IdeHostInput>(input).is_err());
    }

    #[test]
    fn ide_action_rejects_unknown_fields() {
        let input = br#"{"type":"get_editor_context","extra":true}"#;
        assert!(serde_json::from_slice::<IdeAction>(input).is_err());
    }

    #[test]
    fn expected_error_has_a_stable_code() {
        let error = ClspError::new(ErrorCode::BrokerUnavailable, "not running").retryable();
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], "broker_unavailable");
        assert_eq!(value["retryable"], true);
    }
}

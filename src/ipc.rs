#![cfg(windows)]

use std::{
    collections::{BTreeMap, VecDeque},
    ffi::{OsStr, c_void},
    fs,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Stdio,
    ptr::null_mut,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
    process::Command,
    time::{Instant, sleep, timeout, timeout_at},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            ConvertStringSidToSidW, GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, GetTokenInformation, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, GetCurrentProcess, OpenProcessToken,
    },
};

use crate::{
    config::{Config, ConfigOverrides},
    installer::{StatePaths, sanitize_command},
    protocol::{
        BrokerEvent, ClientKind, ClspError, ErrorCode, Hello, IDE_SESSION_ID_HEX_LEN,
        PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, RpcRequest, RpcResponse, WireMessage,
    },
    workspace::Workspace,
};

const AUTH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(1);
const IDE_DISCOVERY_BUDGET: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerMetadata {
    pub protocol: String,
    pub pipe_name: String,
    pub pid: u32,
    pub started_ms: u64,
    pub token: String,
    pub workspace_root: PathBuf,
}

impl BrokerMetadata {
    pub fn new(pipe_name: String, workspace_root: PathBuf) -> Result<Self, ClspError> {
        let mut token = [0u8; 32];
        getrandom::fill(&mut token).map_err(ipc_error)?;
        Ok(Self {
            protocol: PROTOCOL_VERSION.into(),
            pipe_name,
            pid: std::process::id(),
            started_ms: now_ms(),
            token: hex::encode(token),
            workspace_root,
        })
    }
}

#[derive(Clone, Debug)]
pub struct BrokerConnector {
    workspace: PathBuf,
    metadata_path: PathBuf,
    max_frame_bytes: usize,
    client_kind: ClientKind,
}

#[derive(Clone, Debug)]
pub struct IdeRoute {
    connector: BrokerConnector,
    session_id: String,
}

impl IdeRoute {
    pub fn workspace(&self) -> &Path {
        self.connector.workspace()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn request(
        &self,
        request: RpcRequest,
        budget: Duration,
    ) -> Result<RpcResponse, ClspError> {
        self.connector.request_existing(request, budget).await
    }
}

#[derive(Clone, Debug)]
pub struct IdeRouteFailure {
    pub error: ClspError,
    pub user_notice: bool,
}

impl BrokerConnector {
    pub fn for_workspace(path: &Path, client_kind: ClientKind) -> Result<Self, ClspError> {
        let workspace = Workspace::open(path)?;
        let config = Config::load(workspace.root(), ConfigOverrides::default())?;
        config.ensure_enabled()?;
        let paths = StatePaths::for_workspace(&workspace.hash())?;
        Ok(Self::new(
            &workspace,
            &paths,
            config.limits.max_response_bytes,
            client_kind,
        ))
    }

    pub fn new(
        workspace: &Workspace,
        paths: &StatePaths,
        max_frame_bytes: usize,
        client_kind: ClientKind,
    ) -> Self {
        Self {
            workspace: workspace.root().to_path_buf(),
            metadata_path: paths.workspace_state.join("broker.json"),
            max_frame_bytes,
            client_kind,
        }
    }

    pub async fn request(&self, request: RpcRequest) -> Result<RpcResponse, ClspError> {
        let (mut pipe, _) = self.connect_or_spawn().await?;
        self.request_on(&mut pipe, request).await
    }

    pub async fn request_existing(
        &self,
        request: RpcRequest,
        budget: Duration,
    ) -> Result<RpcResponse, ClspError> {
        timeout(budget, async {
            let metadata = load_metadata(&self.metadata_path)?;
            let mut pipe = self
                .connect_authenticated_bounded(&metadata, AUTH_HANDSHAKE_TIMEOUT)
                .await?;
            self.request_on(&mut pipe, request).await
        })
        .await
        .map_err(|_| {
            ClspError::new(
                ErrorCode::BrokerUnavailable,
                "existing Broker request timed out",
            )
            .retryable()
        })?
    }

    async fn request_on(
        &self,
        pipe: &mut NamedPipeClient,
        request: RpcRequest,
    ) -> Result<RpcResponse, ClspError> {
        write_wire(
            pipe,
            &WireMessage::Request(RequestEnvelope {
                id: 1,
                body: request,
            }),
            self.max_frame_bytes,
        )
        .await?;
        match read_wire(pipe, self.max_frame_bytes).await? {
            WireMessage::Response(ResponseEnvelope {
                id: 1,
                result: Some(result),
                error: None,
            }) => Ok(result),
            WireMessage::Response(ResponseEnvelope {
                id: 1,
                error: Some(error),
                ..
            }) => Err(error),
            _ => Err(ipc_error("unexpected Broker response")),
        }
    }

    pub fn containing_existing(
        cwd: &Path,
        client_kind: ClientKind,
    ) -> Result<Vec<Self>, ClspError> {
        let cwd = fs::canonicalize(cwd).map_err(ipc_error)?;
        let root = workspace_state_root()?;
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ipc_error(error)),
        };
        let mut connectors = Vec::new();
        for entry in entries.flatten().take(256) {
            let metadata_path = entry.path().join("broker.json");
            let Ok(metadata) = load_metadata(&metadata_path) else {
                continue;
            };
            let Ok(workspace) = Workspace::open(&metadata.workspace_root) else {
                continue;
            };
            if !workspace.contains_existing(&cwd) {
                continue;
            }
            let Ok(config) = Config::load(workspace.root(), ConfigOverrides::default()) else {
                continue;
            };
            connectors.push(Self {
                workspace: workspace.root().to_path_buf(),
                metadata_path,
                max_frame_bytes: config.limits.max_response_bytes,
                client_kind,
            });
        }
        connectors.sort_by(|left, right| {
            right
                .workspace
                .components()
                .count()
                .cmp(&left.workspace.components().count())
                .then_with(|| left.workspace.cmp(&right.workspace))
        });
        Ok(connectors)
    }

    pub async fn route_ide(
        cwd: &Path,
        session_hint: Option<&str>,
        client_kind: ClientKind,
    ) -> Result<IdeRoute, IdeRouteFailure> {
        if let Some(hint) = session_hint
            && !valid_ide_session_id(hint)
        {
            return Err(route_failure(
                "CLSP IDE session binding is invalid; open a new integrated terminal",
                true,
            ));
        }
        let connectors =
            Self::containing_existing(cwd, client_kind).map_err(|error| IdeRouteFailure {
                error,
                user_notice: session_hint.is_some(),
            })?;
        if connectors.is_empty() {
            return Err(route_failure(
                "no live CLSP IDE session is available",
                session_hint.is_some(),
            ));
        }

        let canonical_cwd = fs::canonicalize(cwd).map_err(|error| IdeRouteFailure {
            error: ipc_error(error),
            user_notice: session_hint.is_some(),
        })?;
        let expected = connectors.len();
        let mut requests = FuturesUnordered::new();
        for connector in connectors {
            let request_cwd = canonical_cwd.clone();
            requests.push(async move {
                let response = connector
                    .request_existing(
                        RpcRequest::ListIdeCandidates { cwd: request_cwd },
                        IDE_DISCOVERY_BUDGET,
                    )
                    .await;
                (connector, response)
            });
        }

        let deadline = Instant::now() + IDE_DISCOVERY_BUDGET;
        let mut completed = 0usize;
        let mut valid_responses = true;
        let mut candidates = BTreeMap::<String, BrokerConnector>::new();
        while !requests.is_empty() {
            let Ok(Some((connector, response))) = timeout_at(deadline, requests.next()).await
            else {
                break;
            };
            completed += 1;
            let Ok(RpcResponse::IdeCandidates {
                candidates: response_candidates,
            }) = response
            else {
                valid_responses = false;
                continue;
            };
            for candidate in response_candidates {
                if !valid_ide_session_id(&candidate.session_id)
                    || candidate.workspace_root != connector.workspace
                {
                    valid_responses = false;
                    continue;
                }
                match candidates.entry(candidate.session_id) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(connector.clone());
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        if prefer_workspace(&entry.get().workspace, &connector.workspace) {
                            entry.insert(connector.clone());
                        }
                    }
                }
            }
        }

        let session_id = choose_ide_session(
            &candidates,
            session_hint,
            completed == expected && valid_responses,
        )
            .map_err(|choice| match choice {
                RouteChoice::Unavailable => {
                    route_failure("no live CLSP IDE session is available", false)
                }
                RouteChoice::Ambiguous => route_failure(
                    "multiple VS Code windows match this workspace; start a new integrated terminal in the intended window",
                    true,
                ),
                RouteChoice::InvalidHint => route_failure(
                    "the bound VS Code window is no longer available; start a new integrated terminal",
                    true,
                ),
            })?
            .to_owned();
        let connector = candidates
            .remove(&session_id)
            .expect("selected IDE candidate exists");
        Ok(IdeRoute {
            connector,
            session_id,
        })
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub async fn subscribe(&self, after_seq: u64) -> Result<BrokerSubscription, ClspError> {
        let (mut pipe, _) = self.connect_or_spawn().await?;
        write_wire(
            &mut pipe,
            &WireMessage::Request(RequestEnvelope {
                id: 1,
                body: RpcRequest::Subscribe { after_seq },
            }),
            self.max_frame_bytes,
        )
        .await?;
        let pending = match read_wire(&mut pipe, self.max_frame_bytes).await? {
            WireMessage::Response(ResponseEnvelope {
                id: 1,
                result: Some(RpcResponse::Events { events }),
                error: None,
            }) => events.into(),
            WireMessage::Response(ResponseEnvelope {
                id: 1,
                error: Some(error),
                ..
            }) => return Err(error),
            _ => return Err(ipc_error("unexpected Broker subscription response")),
        };
        Ok(BrokerSubscription {
            pipe,
            max_frame_bytes: self.max_frame_bytes,
            last_seq: after_seq,
            pending,
        })
    }

    pub async fn connect_or_spawn(&self) -> Result<(NamedPipeClient, BrokerMetadata), ClspError> {
        match load_metadata(&self.metadata_path) {
            Ok(metadata) => match self
                .connect_authenticated_bounded(&metadata, AUTH_HANDSHAKE_TIMEOUT)
                .await
            {
                Ok(pipe) => return Ok((pipe, metadata)),
                Err(error) if error.code == ErrorCode::ProtocolMismatch => return Err(error),
                Err(_) => {}
            },
            Err(error) if error.code == ErrorCode::ProtocolMismatch => return Err(error),
            Err(_) => {}
        }
        self.spawn_broker()?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(metadata) = load_metadata(&self.metadata_path)
                && let Ok(pipe) = self
                    .connect_authenticated_bounded(&metadata, AUTH_HANDSHAKE_TIMEOUT)
                    .await
            {
                return Ok((pipe, metadata));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ClspError::new(
                    ErrorCode::BrokerUnavailable,
                    "Broker did not become available within the startup window",
                )
                .retryable());
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn connect_authenticated_bounded(
        &self,
        metadata: &BrokerMetadata,
        limit: Duration,
    ) -> Result<NamedPipeClient, ClspError> {
        timeout(limit, self.connect_authenticated(metadata))
            .await
            .map_err(|_| ipc_error("Broker authentication handshake timed out"))?
    }

    async fn connect_authenticated(
        &self,
        metadata: &BrokerMetadata,
    ) -> Result<NamedPipeClient, ClspError> {
        if metadata.protocol != PROTOCOL_VERSION {
            return Err(ClspError::new(
                ErrorCode::ProtocolMismatch,
                "Broker protocol version does not match this client",
            ));
        }
        let mut pipe = ClientOptions::new()
            .open(&metadata.pipe_name)
            .map_err(ipc_error)?;
        write_wire(
            &mut pipe,
            &WireMessage::Hello(Hello {
                protocol: PROTOCOL_VERSION.into(),
                token: metadata.token.clone(),
                client_kind: self.client_kind,
            }),
            self.max_frame_bytes,
        )
        .await?;
        match read_wire(&mut pipe, self.max_frame_bytes).await? {
            WireMessage::Response(ResponseEnvelope {
                id: 0,
                result: Some(RpcResponse::Ack),
                error: None,
            }) => Ok(pipe),
            WireMessage::Response(ResponseEnvelope {
                error: Some(error), ..
            }) => Err(error),
            _ => Err(ipc_error("Broker authentication handshake failed")),
        }
    }

    fn spawn_broker(&self) -> Result<(), ClspError> {
        let executable = std::env::current_exe().map_err(ipc_error)?;
        let mut command = Command::new(executable);
        command
            .arg("broker")
            .arg("--workspace")
            .arg(&self.workspace);
        if self.client_kind == ClientKind::Ide {
            command.arg("--defer-prewarm");
        }
        sanitize_command(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false)
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        command.spawn().map_err(ipc_error)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteChoice {
    Unavailable,
    Ambiguous,
    InvalidHint,
}

fn choose_ide_session<'a, T>(
    candidates: &'a BTreeMap<String, T>,
    session_hint: Option<&str>,
    discovery_complete: bool,
) -> Result<&'a str, RouteChoice> {
    if let Some(hint) = session_hint {
        if !valid_ide_session_id(hint) {
            return Err(RouteChoice::InvalidHint);
        }
        return candidates
            .get_key_value(hint)
            .map(|(session_id, _)| session_id.as_str())
            .ok_or(RouteChoice::InvalidHint);
    }
    if !discovery_complete {
        return Err(RouteChoice::Unavailable);
    }
    match candidates.len() {
        0 => Err(RouteChoice::Unavailable),
        1 => Ok(candidates.first_key_value().unwrap().0),
        _ => Err(RouteChoice::Ambiguous),
    }
}

fn valid_ide_session_id(session_id: &str) -> bool {
    session_id.len() == IDE_SESSION_ID_HEX_LEN
        && session_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn prefer_workspace(current: &Path, candidate: &Path) -> bool {
    candidate.components().count() > current.components().count()
        || (candidate.components().count() == current.components().count() && candidate < current)
}

fn route_failure(message: &str, user_notice: bool) -> IdeRouteFailure {
    IdeRouteFailure {
        error: ClspError::new(ErrorCode::IdeUnavailable, message).retryable(),
        user_notice,
    }
}

pub struct BrokerSubscription {
    pipe: NamedPipeClient,
    max_frame_bytes: usize,
    last_seq: u64,
    pending: VecDeque<BrokerEvent>,
}

impl BrokerSubscription {
    pub async fn next(&mut self) -> Result<BrokerEvent, ClspError> {
        loop {
            let event = if let Some(event) = self.pending.pop_front() {
                event
            } else {
                match read_wire(&mut self.pipe, self.max_frame_bytes).await? {
                    WireMessage::Event(event) => BrokerEvent {
                        seq: event.seq,
                        timestamp_ms: event.timestamp_ms,
                        body: event.body,
                    },
                    _ => return Err(ipc_error("unexpected Broker subscription frame")),
                }
            };
            if event.seq > self.last_seq {
                self.last_seq = event.seq;
                return Ok(event);
            }
        }
    }
}

pub fn pipe_name(workspace: &Workspace) -> Result<String, ClspError> {
    let sid = current_user_sid_string()?;
    let sid_hash = sha2::Sha256::digest(sid.as_bytes());
    Ok(format!(
        r"\\.\pipe\clsp-{}-{}",
        &hex::encode(sid_hash)[..16],
        &workspace.hash()[..24]
    ))
}

pub fn create_pipe_server(name: &str, first: bool) -> Result<NamedPipeServer, ClspError> {
    let descriptor = SecurityDescriptor::for_current_user(false)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(254);
    // The descriptor remains alive until CreateNamedPipeW returns.
    unsafe {
        options.create_with_security_attributes_raw(
            name,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
        )
    }
    .map_err(ipc_error)
}

pub async fn authenticate_server(
    pipe: &mut NamedPipeServer,
    token: &str,
    max_frame_bytes: usize,
) -> Result<ClientKind, ClspError> {
    let hello = read_wire(pipe, max_frame_bytes).await;
    let result = match hello {
        Ok(WireMessage::Hello(Hello {
            protocol,
            token: supplied,
            client_kind,
        })) if protocol == PROTOCOL_VERSION
            && constant_time_eq(supplied.as_bytes(), token.as_bytes()) =>
        {
            write_wire(
                pipe,
                &WireMessage::Response(ResponseEnvelope {
                    id: 0,
                    result: Some(RpcResponse::Ack),
                    error: None,
                }),
                max_frame_bytes,
            )
            .await?;
            Ok(client_kind)
        }
        Ok(WireMessage::Hello(_)) => Err(ClspError::new(
            ErrorCode::AuthenticationFailed,
            "invalid Broker protocol or authentication token",
        )),
        Ok(_) => Err(ClspError::new(
            ErrorCode::AuthenticationFailed,
            "the first Broker frame must be a hello message",
        )),
        Err(error) => Err(error),
    };
    if let Err(error) = &result {
        let _ = write_wire(
            pipe,
            &WireMessage::Response(ResponseEnvelope {
                id: 0,
                result: None,
                error: Some(error.clone()),
            }),
            max_frame_bytes,
        )
        .await;
    }
    result
}

pub async fn read_wire<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_frame_bytes: usize,
) -> Result<WireMessage, ClspError> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).await.map_err(ipc_error)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > max_frame_bytes {
        return Err(ipc_error(
            "Broker frame length is outside configured bounds",
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await.map_err(ipc_error)?;
    serde_json::from_slice(&body).map_err(ipc_error)
}

pub async fn write_wire<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &WireMessage,
    max_frame_bytes: usize,
) -> Result<(), ClspError> {
    let body = serde_json::to_vec(message).map_err(ipc_error)?;
    if body.is_empty() || body.len() > max_frame_bytes || body.len() > u32::MAX as usize {
        return Err(ipc_error("Broker frame exceeds configured bounds"));
    }
    writer
        .write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(ipc_error)?;
    writer.write_all(&body).await.map_err(ipc_error)?;
    writer.flush().await.map_err(ipc_error)
}

pub async fn publish_metadata(path: &Path, metadata: &BrokerMetadata) -> Result<(), ClspError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(ipc_error)?;
        apply_user_system_dacl(parent, true)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(metadata).map_err(ipc_error)?;
    tokio::fs::write(&temp, bytes).await.map_err(ipc_error)?;
    apply_user_system_dacl(&temp, false)?;
    atomic_replace(&temp, path).map_err(ipc_error)?;
    verify_user_system_dacl(path)?;
    Ok(())
}

pub fn load_metadata(path: &Path) -> Result<BrokerMetadata, ClspError> {
    verify_user_system_dacl(path)?;
    let bytes = std::fs::read(path).map_err(ipc_error)?;
    if bytes.len() > 64 * 1024 {
        return Err(ipc_error("broker metadata exceeds limit"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(ipc_error)?;
    if value.get("protocol").and_then(serde_json::Value::as_str) != Some(PROTOCOL_VERSION) {
        return Err(ClspError::new(
            ErrorCode::ProtocolMismatch,
            "Broker protocol version does not match this client; restart CLSP processes",
        ));
    }
    let metadata: BrokerMetadata = serde_json::from_value(value).map_err(ipc_error)?;
    if metadata.token.len() != 64
        || !metadata.token.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !metadata.workspace_root.is_absolute()
    {
        return Err(ipc_error("broker metadata is invalid"));
    }
    Ok(metadata)
}

fn workspace_state_root() -> Result<PathBuf, ClspError> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        ClspError::new(
            ErrorCode::InvalidConfig,
            "LOCALAPPDATA is required on Windows",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("clsp")
        .join("state")
        .join("workspaces"))
}

pub fn apply_user_system_dacl(path: &Path, inheritable: bool) -> Result<(), ClspError> {
    let descriptor = SecurityDescriptor::for_current_user(inheritable)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = null_mut();
    // The DACL pointer remains owned by descriptor for this call.
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
        || present == 0
        || dacl.is_null()
    {
        return Err(last_ipc_error("cannot read generated DACL"));
    }
    let mut wide = wide_path(path);
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    if status != 0 {
        return Err(ipc_error(format!(
            "cannot protect state path, Win32 error {status}"
        )));
    }
    verify_user_system_dacl(path)
}

pub fn verify_user_system_dacl(path: &Path) -> Result<(), ClspError> {
    let user_sid = AllocatedSid::from_string(&current_user_sid_string()?)?;
    let system_sid = AllocatedSid::from_string("S-1-5-18")?;
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let wide = wide_path(path);
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() || owner.is_null() || dacl.is_null() {
        if !descriptor.is_null() {
            unsafe { LocalFree(descriptor) };
        }
        return Err(ClspError::new(
            ErrorCode::AuthenticationFailed,
            format!("cannot inspect Broker state ACL, Win32 error {status}"),
        ));
    }
    let descriptor_guard = LocalAllocation(descriptor);
    if unsafe { EqualSid(owner, user_sid.0) } == 0 {
        return Err(ClspError::new(
            ErrorCode::AuthenticationFailed,
            "Broker state owner is not the current user",
        ));
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe { GetSecurityDescriptorControl(descriptor_guard.0, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err(ClspError::new(
            ErrorCode::AuthenticationFailed,
            "Broker state DACL is not protected",
        ));
    }
    let ace_count = unsafe { (*dacl).AceCount } as u32;
    if ace_count < 2 {
        return Err(ClspError::new(
            ErrorCode::AuthenticationFailed,
            "Broker state DACL principal set is incomplete",
        ));
    }
    let mut user_count = 0;
    let mut system_count = 0;
    for index in 0..ace_count {
        let mut raw_ace: *mut c_void = null_mut();
        if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 || raw_ace.is_null() {
            return Err(last_auth_error("cannot enumerate Broker state DACL"));
        }
        let ace = raw_ace.cast::<ACCESS_ALLOWED_ACE>();
        if unsafe { (*ace).Header.AceType } != 0 {
            return Err(last_auth_error(
                "Broker state DACL contains a non-allow ACE",
            ));
        }
        let sid = unsafe { (&mut (*ace).SidStart as *mut u32).cast::<c_void>() };
        if unsafe { EqualSid(sid, user_sid.0) } != 0 {
            user_count += 1;
        } else if unsafe { EqualSid(sid, system_sid.0) } != 0 {
            system_count += 1;
        } else {
            return Err(last_auth_error(
                "Broker state DACL contains an unexpected SID",
            ));
        }
    }
    if user_count == 0 || system_count == 0 {
        return Err(last_auth_error(
            "Broker state DACL principal set is incomplete",
        ));
    }
    Ok(())
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn for_current_user(inheritable: bool) -> Result<Self, ClspError> {
        let sid = current_user_sid_string()?;
        let flags = if inheritable { "OICI" } else { "" };
        let sddl = format!("D:P(A;{flags};GA;;;SY)(A;{flags};GA;;;{sid})");
        let wide = wide_string(&sddl);
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
            || descriptor.is_null()
        {
            return Err(last_ipc_error("cannot build protected security descriptor"));
        }
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

struct AllocatedSid(PSID);

impl AllocatedSid {
    fn from_string(value: &str) -> Result<Self, ClspError> {
        let wide = wide_string(value);
        let mut sid: PSID = null_mut();
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 || sid.is_null() {
            return Err(last_ipc_error("cannot parse SID"));
        }
        Ok(Self(sid))
    }
}

impl Drop for AllocatedSid {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn current_user_sid_string() -> Result<String, ClspError> {
    let mut token: HANDLE = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_ipc_error("cannot open current process token"));
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let token = Token(token);
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(last_ipc_error("cannot size current user token"));
    }
    let words = (needed as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; words];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(last_ipc_error("cannot read current user token"));
    }
    let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let mut string_sid = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut string_sid) } == 0 || string_sid.is_null() {
        return Err(last_ipc_error("cannot format current user SID"));
    }
    let string_guard = LocalAllocation(string_sid.cast::<c_void>());
    let mut length = 0;
    while unsafe { *string_sid.add(length) } != 0 {
        length += 1;
    }
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(string_sid, length) })
        .map_err(ipc_error)?;
    drop(string_guard);
    Ok(value)
}

pub(crate) fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = wide_path(source);
    let destination = wide_path(destination);
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn wide_string(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn last_ipc_error(context: &str) -> ClspError {
    let code = unsafe { GetLastError() };
    ipc_error(format!("{context}, Win32 error {code}"))
}

fn last_auth_error(context: &str) -> ClspError {
    let code = unsafe { GetLastError() };
    ClspError::new(
        ErrorCode::AuthenticationFailed,
        format!("{context}, Win32 error {code}"),
    )
}

fn ipc_error(error: impl std::fmt::Display) -> ClspError {
    ClspError::new(ErrorCode::BrokerUnavailable, error.to_string()).retryable()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("broker.json");
        std::fs::write(&file, b"{}").unwrap();
        apply_user_system_dacl(&file, false).unwrap();
        verify_user_system_dacl(&file).unwrap();

        let inherited = directory.path().join("state");
        std::fs::create_dir(&inherited).unwrap();
        apply_user_system_dacl(&inherited, true).unwrap();
        verify_user_system_dacl(&inherited).unwrap();
    }
}

use std::path::Path;

use rmcp::{
    ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::{Config, ConfigOverrides},
    installer::StatePaths,
    ipc::BrokerConnector,
    protocol::{
        ClientKind, ClspError, DiagnosticSeverity, ErrorCode, Position, QueryOperation,
        QueryRequest, RpcRequest, RpcResponse,
    },
    workspace::Workspace,
};

#[derive(Clone, Debug)]
struct McpService {
    connector: BrokerConnector,
    workspace: Workspace,
    ide_session_hint: Option<String>,
    max_file_bytes: u64,
    max_diagnostic_files: usize,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryInput {
    operation: QueryOperation,
    file: std::path::PathBuf,
    line: u32,
    character: u32,
    #[serde(default)]
    include_declaration: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DiagnosticsInput {
    files: Vec<std::path::PathBuf>,
    minimum_severity: Option<DiagnosticSeverity>,
    wait_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdeDiagnosticsInput {
    #[serde(default)]
    file: Option<std::path::PathBuf>,
    #[serde(default)]
    minimum_severity: Option<DiagnosticSeverity>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StatusInput {
    #[serde(default)]
    verbose: bool,
}

impl McpService {
    fn new(connector: BrokerConnector, workspace: Workspace, config: &Config) -> Self {
        Self {
            connector,
            workspace,
            ide_session_hint: ide_session_hint(),
            max_file_bytes: config.limits.max_file_bytes,
            max_diagnostic_files: config.diagnostics.max_files,
            tool_router: Self::tool_router(),
        }
    }

    async fn request(&self, request: RpcRequest) -> CallToolResult {
        match self.connector.request(request).await {
            Ok(response) => structured(response),
            Err(error) => structured_error(error),
        }
    }
}

#[tool_router]
impl McpService {
    #[tool(
        description = "Query hover, definition, or references for a workspace file",
        annotations(title = "LSP query", read_only_hint = true)
    )]
    async fn lsp_query(&self, Parameters(input): Parameters<QueryInput>) -> CallToolResult {
        match query_request(input, &self.workspace, self.max_file_bytes) {
            Ok(request) => self.request(request).await,
            Err(error) => structured_error(error),
        }
    }

    #[tool(
        description = "Return bounded diagnostics for one or more workspace files",
        annotations(title = "LSP diagnostics", read_only_hint = true)
    )]
    async fn lsp_diagnostics(
        &self,
        Parameters(input): Parameters<DiagnosticsInput>,
    ) -> CallToolResult {
        if input.files.is_empty() || input.files.len() > self.max_diagnostic_files {
            return structured_error(ClspError::new(
                ErrorCode::InvalidRequest,
                "diagnostics path count is outside configured bounds",
            ));
        }
        let paths = match input
            .files
            .into_iter()
            .map(|path| self.workspace.resolve_file(path, self.max_file_bytes))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(paths) => paths,
            Err(error) => return structured_error(error),
        };
        self.request(RpcRequest::Diagnostics {
            paths,
            minimum_severity: input.minimum_severity,
            wait_ms: input.wait_ms,
        })
        .await
    }

    #[tool(
        description = "Read the current VS Code Problems diagnostics for this workspace",
        annotations(
            title = "IDE diagnostics",
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn ide_diagnostics(
        &self,
        Parameters(input): Parameters<IdeDiagnosticsInput>,
    ) -> CallToolResult {
        let file = match input
            .file
            .map(|path| self.workspace.resolve_file(path, self.max_file_bytes))
            .transpose()
        {
            Ok(file) => file,
            Err(error) => return structured_error(error),
        };
        let route = match BrokerConnector::route_ide(
            self.workspace.root(),
            self.ide_session_hint.as_deref(),
            ClientKind::Mcp,
        )
        .await
        {
            Ok(route) => route,
            Err(failure) => return structured_error(failure.error),
        };
        match route
            .request(
                RpcRequest::GetIdeDiagnostics {
                    session_id: route.session_id().to_owned(),
                    file,
                    minimum_severity: input.minimum_severity,
                },
                std::time::Duration::from_millis(2_200),
            )
            .await
        {
            Ok(response) => structured(response),
            Err(error) => structured_error(error),
        }
    }

    #[tool(
        description = "Return the CLSP Broker, language-server, hook, and recent failure status",
        annotations(title = "LSP status", read_only_hint = true)
    )]
    async fn lsp_status(&self, Parameters(input): Parameters<StatusInput>) -> CallToolResult {
        match self.connector.request(RpcRequest::Snapshot).await {
            Ok(RpcResponse::Snapshot(mut snapshot)) => {
                if !input.verbose {
                    snapshot.recent_events.clear();
                }
                structured(RpcResponse::Snapshot(snapshot))
            }
            Ok(response) => structured(response),
            Err(error) => structured_error(error),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for McpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Use lsp_query for navigation. Prefer ide_diagnostics for current VS Code Problems; use lsp_diagnostics for an explicit independent or no-IDE check. Use lsp_status for availability.",
        )
    }

    fn on_initialized(
        &self,
        _context: rmcp::service::NotificationContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        let connector = self.connector.clone();
        async move {
            let _ = connector.request(RpcRequest::Discover).await;
        }
    }
}

fn ide_session_hint() -> Option<String> {
    std::env::var_os("CLSP_IDE_SESSION_ID").map(|value| value.into_string().unwrap_or_default())
}

pub async fn run(workspace_path: &Path) -> anyhow::Result<()> {
    let workspace = Workspace::open(workspace_path)?;
    let config = Config::load(workspace.root(), ConfigOverrides::default())?;
    config.ensure_enabled()?;
    let paths = StatePaths::for_workspace(&workspace.hash())?;
    let connector = BrokerConnector::new(
        &workspace,
        &paths,
        config.limits.max_response_bytes,
        ClientKind::Mcp,
    );
    let service = McpService::new(connector, workspace, &config);
    service
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

fn structured(value: impl Serialize) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => structured_error(ClspError::new(
            crate::protocol::ErrorCode::Internal,
            error.to_string(),
        )),
    }
}

fn structured_error(error: ClspError) -> CallToolResult {
    CallToolResult::structured_error(
        serde_json::to_value(error).expect("ClspError always serializes"),
    )
}

fn query_request(
    input: QueryInput,
    workspace: &Workspace,
    max_file_bytes: u64,
) -> Result<RpcRequest, ClspError> {
    if input.line == 0 || input.character == 0 {
        return Err(ClspError::new(
            crate::protocol::ErrorCode::InvalidRequest,
            "line and character are one-based and must be greater than zero",
        ));
    }
    Ok(RpcRequest::Query(QueryRequest {
        operation: input.operation,
        path: workspace.resolve_file(input.file, max_file_bytes)?,
        position: Position {
            line: input.line,
            column: input.character,
        },
        include_declaration: input.include_declaration,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_input_rejects_zero_based_positions() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("src")).unwrap();
        std::fs::write(directory.path().join("src/lib.rs"), "").unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let input: QueryInput = serde_json::from_value(serde_json::json!({
            "operation": "definition",
            "file": "src/lib.rs",
            "line": 0,
            "character": 1
        }))
        .unwrap();
        assert_eq!(
            query_request(input, &workspace, 1024).unwrap_err().code,
            crate::protocol::ErrorCode::InvalidRequest
        );
    }
}

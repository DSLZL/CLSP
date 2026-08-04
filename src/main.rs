use anyhow::Result;
use clap::Parser;
use clsp::cli::{Cli, Command, HookCommand};
use clsp::{
    ipc::BrokerConnector,
    protocol::{ClientKind, RpcRequest},
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup { workspace } => clsp::setup::run(&workspace).await?,
        Command::Mcp { workspace } => clsp::mcp::run(&workspace).await?,
        Command::Broker {
            workspace,
            defer_prewarm,
        } => clsp::broker::run(&workspace, !defer_prewarm).await?,
        Command::IdeHost {
            workspace,
            session_id,
        } => clsp::ide::run(&workspace, &session_id).await?,
        Command::Hook { command } => match command {
            HookCommand::SessionStart
            | HookCommand::UserPrompt
            | HookCommand::PreTool
            | HookCommand::PostTool
            | HookCommand::SessionEnd => clsp::hook::run(command).await?,
        },
        Command::Status { workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            let connector = BrokerConnector::for_workspace(&workspace, ClientKind::Status)?;
            let response = connector.request(RpcRequest::Snapshot).await?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Tui { workspace } => {
            let workspace = workspace.unwrap_or(std::env::current_dir()?);
            clsp::tui::run(&workspace).await?;
        }
    }
    Ok(())
}

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "clsp", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the bundled VS Code adapter and configure this Codex project.
    Setup {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Run the stdio MCP adapter.
    Mcp {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Run the per-workspace background broker.
    #[command(hide = true)]
    Broker {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        defer_prewarm: bool,
    },
    /// Relay the bundled VS Code adapter to a workspace Broker.
    #[command(hide = true)]
    IdeHost {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        session_id: String,
    },
    /// Run a Codex lifecycle hook.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Print the current broker snapshot.
    Status {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Attach the terminal overview.
    Tui {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, Subcommand, PartialEq, Eq)]
#[command(rename_all = "kebab-case")]
pub enum HookCommand {
    SessionStart,
    UserPrompt,
    PreTool,
    PostTool,
    SessionEnd,
}

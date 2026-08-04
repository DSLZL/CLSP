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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_approved_command_surface() {
        let cli = Cli::try_parse_from(["clsp", "setup", "--workspace", "."]).unwrap();
        assert!(matches!(cli.command, Command::Setup { .. }));

        let cli = Cli::try_parse_from(["clsp", "mcp", "--workspace", "."]).unwrap();
        assert!(matches!(cli.command, Command::Mcp { .. }));

        let cli =
            Cli::try_parse_from(["clsp", "broker", "--workspace", ".", "--defer-prewarm"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Broker {
                defer_prewarm: true,
                ..
            }
        ));

        let cli = Cli::try_parse_from([
            "clsp",
            "ide-host",
            "--workspace",
            ".",
            "--session-id",
            &"a".repeat(64),
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::IdeHost { .. }));

        let cli = Cli::try_parse_from(["clsp", "hook", "post-tool"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Hook {
                command: HookCommand::PostTool
            }
        ));

        let cli = Cli::try_parse_from(["clsp", "hook", "user-prompt"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Hook {
                command: HookCommand::UserPrompt
            }
        ));

        let cli = Cli::try_parse_from(["clsp", "hook", "pre-tool"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Hook {
                command: HookCommand::PreTool
            }
        ));

        assert!(Cli::try_parse_from(["clsp", "preflight"]).is_err());
    }
}

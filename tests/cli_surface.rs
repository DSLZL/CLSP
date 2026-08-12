use clap::Parser;
use clsp::cli::{Cli, Command, HookCommand};

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

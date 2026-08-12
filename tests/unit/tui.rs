use ratatui::{Terminal, backend::TestBackend};

use super::*;

#[test]
fn overview_renders_in_a_bounded_terminal() {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let app = App {
        snapshot: BrokerSnapshot {
            protocol: "clsp-rpc/2".into(),
            workspace: "C:/workspace".into(),
            broker_pid: 7,
            sequence: 0,
            servers: Vec::new(),
            active_connections: 1,
            active_leases: 0,
            active_ide_sessions: 0,
            hook_last_seen_ms: None,
            hook_same_turn_ready: false,
            recent_events: Vec::new(),
        },
        selected: 0,
        connected: true,
        event_limit: 10,
    };
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("CLSP Overview"));
    assert!(text.contains("No language servers detected yet"));
}

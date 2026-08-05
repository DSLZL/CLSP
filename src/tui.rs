use std::{collections::VecDeque, path::Path, time::Duration};

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures_util::StreamExt;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table, Wrap},
};

use crate::{
    config::{Config, ConfigOverrides},
    ipc::{BrokerConnector, BrokerSubscription},
    protocol::{
        BrokerEvent, BrokerSnapshot, ClientKind, EventBody, RpcRequest, RpcResponse, ServerState,
    },
    workspace::Workspace,
};

struct App {
    snapshot: BrokerSnapshot,
    selected: usize,
    connected: bool,
    event_limit: usize,
}

impl App {
    fn replace_snapshot(&mut self, snapshot: BrokerSnapshot) {
        self.snapshot = snapshot;
        self.selected = self
            .selected
            .min(self.snapshot.servers.len().saturating_sub(1));
        self.connected = true;
        self.trim_events();
    }

    fn apply_event(&mut self, event: BrokerEvent) {
        if event.seq <= self.snapshot.sequence {
            return;
        }
        self.snapshot.sequence = event.seq;
        match &event.body {
            EventBody::ServerState { key, state, detail } => {
                if let Some(server) = self
                    .snapshot
                    .servers
                    .iter_mut()
                    .find(|item| item.key == *key)
                {
                    server.state = *state;
                    server.detail.clone_from(detail);
                    if *state != ServerState::Installing {
                        server.install_progress = None;
                    }
                }
            }
            EventBody::InstallProgress {
                server_id,
                progress,
            } => {
                for server in self
                    .snapshot
                    .servers
                    .iter_mut()
                    .filter(|item| item.key.server_id == *server_id)
                {
                    server.install_progress = Some(*progress);
                }
            }
            EventBody::LeaseChanged { .. } => {}
            EventBody::DiagnosticsChanged { .. } | EventBody::BrokerMessage { .. } => {}
        }
        self.snapshot.recent_events.push(event);
        self.trim_events();
    }

    fn trim_events(&mut self) {
        let excess = self
            .snapshot
            .recent_events
            .len()
            .saturating_sub(self.event_limit);
        self.snapshot.recent_events.drain(..excess);
    }

    fn select_next(&mut self) {
        if !self.snapshot.servers.is_empty() {
            self.selected = (self.selected + 1).min(self.snapshot.servers.len() - 1);
        }
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

pub async fn run(workspace_path: &Path) -> anyhow::Result<()> {
    let workspace = Workspace::open(workspace_path)?;
    let config = Config::load(workspace.root(), ConfigOverrides::default())?;
    config.ensure_enabled()?;
    let connector = BrokerConnector::for_workspace(workspace.root(), ClientKind::Tui)?;
    let (snapshot, mut subscription) = attach(&connector).await?;
    let mut app = App {
        snapshot,
        selected: 0,
        connected: true,
        event_limit: config.tui.recent_events,
    };

    let mut terminal = ratatui::try_init()?;
    let _restore = RestoreTerminal;
    let mut terminal_events = EventStream::new();
    let refresh_ms = (1_000u64 / u64::from(config.tui.refresh_hz_active.max(1))).max(25);
    let mut tick = tokio::time::interval(Duration::from_millis(refresh_ms));
    let mut full_refresh = tokio::time::interval(Duration::from_secs(3));
    tick.tick().await;
    full_refresh.tick().await;
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|frame| render(frame, &app))?;
            dirty = false;
        }
        tokio::select! {
            event = terminal_events.next() => {
                let Some(Ok(Event::Key(key))) = event else { continue };
                if key.kind != KeyEventKind::Press { continue; }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.select_previous();
                        dirty = true;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.select_next();
                        dirty = true;
                    }
                    KeyCode::Char('r') => control_selected(&connector, &app, true),
                    KeyCode::Char('s') => control_selected(&connector, &app, false),
                    _ => {}
                }
            }
            result = next_event(&mut subscription) => {
                match result {
                    Ok(event) => {
                        app.apply_event(event);
                        dirty = true;
                    }
                    Err(_) => {
                        app.connected = false;
                        subscription = None;
                        dirty = true;
                    }
                }
            }
            _ = full_refresh.tick() => {
                match attach(&connector).await {
                    Ok((snapshot, next_subscription)) => {
                        app.replace_snapshot(snapshot);
                        subscription = next_subscription;
                        dirty = true;
                    }
                    Err(_) => {
                        app.connected = false;
                        dirty = true;
                    }
                }
            }
            _ = tick.tick() => {
                dirty |= app.snapshot.servers.iter().any(|server| server.install_progress.is_some());
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

async fn attach(
    connector: &BrokerConnector,
) -> Result<(BrokerSnapshot, Option<BrokerSubscription>), crate::protocol::ClspError> {
    let snapshot = match connector.request(RpcRequest::Snapshot).await? {
        RpcResponse::Snapshot(snapshot) => snapshot,
        _ => {
            return Err(crate::protocol::ClspError::new(
                crate::protocol::ErrorCode::BrokerUnavailable,
                "Broker returned an unexpected snapshot response",
            ));
        }
    };
    let subscription = connector.subscribe(snapshot.sequence).await?;
    Ok((snapshot, Some(subscription)))
}

async fn next_event(
    subscription: &mut Option<BrokerSubscription>,
) -> Result<BrokerEvent, crate::protocol::ClspError> {
    match subscription {
        Some(subscription) => subscription.next().await,
        None => std::future::pending().await,
    }
}

fn control_selected(connector: &BrokerConnector, app: &App, retry: bool) {
    let Some(server) = app.snapshot.servers.get(app.selected) else {
        return;
    };
    let request = if retry {
        RpcRequest::RetryServer {
            key: server.key.clone(),
        }
    } else if server.state == ServerState::Running {
        RpcRequest::StopServer {
            key: server.key.clone(),
        }
    } else {
        RpcRequest::StartServer {
            key: server.key.clone(),
        }
    };
    let connector = connector.clone();
    tokio::spawn(async move {
        let _ = connector.request(request).await;
    });
}

fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .split(area);

    let connection = if app.connected {
        Span::styled("connected", Style::default().fg(Color::Green))
    } else {
        Span::styled("reconnecting", Style::default().fg(Color::Yellow))
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "CLSP Overview  ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            connection,
            Span::raw(format!(
                "  broker {}  leases {}  clients {}",
                app.snapshot.broker_pid,
                app.snapshot.active_leases,
                app.snapshot.active_connections
            )),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        sections[0],
    );

    let rows = app
        .snapshot
        .servers
        .iter()
        .enumerate()
        .map(|(index, server)| {
            let progress = server
                .install_progress
                .map(|value| if value >= 1.0 { "done" } else { "running" }.to_owned())
                .unwrap_or_default();
            let row = Row::new(vec![
                server.key.server_id.clone(),
                server.key.root.display().to_string(),
                format!("{:?}", server.state),
                server.key.artifact_version.clone(),
                server.pid.map(|pid| pid.to_string()).unwrap_or_default(),
                progress,
            ]);
            if index == app.selected {
                row.style(Style::default().fg(Color::Black).bg(Color::Cyan))
            } else {
                row
            }
        });
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Percentage(42),
            Constraint::Length(11),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(8),
        ],
    )
    .header(
        Row::new(["Server", "Root", "State", "Version", "PID", "Install"])
            .style(Style::default().fg(Color::Yellow).bold()),
    )
    .column_spacing(1)
    .block(Block::default().title("Servers").borders(Borders::ALL));
    frame.render_widget(table, sections[1]);

    let detail = app
        .snapshot
        .servers
        .get(app.selected)
        .map(|server| {
            format!(
                "Executable: {}\nDetail: {}",
                server
                    .executable
                    .as_deref()
                    .map(Path::display)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "not resolved".into()),
                server.detail.as_deref().unwrap_or("none")
            )
        })
        .unwrap_or_else(|| "No language servers detected yet.".into());
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: true })
            .block(Block::default().title("Selected").borders(Borders::ALL)),
        sections[2],
    );

    let event_width = area.width.saturating_sub(2) as usize;
    let events: VecDeque<_> = app
        .snapshot
        .recent_events
        .iter()
        .rev()
        .take(sections[3].height.saturating_sub(2) as usize)
        .map(|event| {
            truncate(
                &format!("{:>5} {}", event.seq, event_label(&event.body)),
                event_width,
            )
        })
        .collect();
    let event_lines = events.into_iter().map(Line::from).collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(event_lines).block(
            Block::default()
                .title("Recent events")
                .borders(Borders::ALL),
        ),
        sections[3],
    );
    frame.render_widget(
        Paragraph::new("q quit  arrows select  r retry  s start/stop").fg(Color::Gray),
        sections[4],
    );
}

fn event_label(event: &EventBody) -> String {
    match event {
        EventBody::ServerState { key, state, .. } => {
            format!("{} -> {state:?}", key.server_id)
        }
        EventBody::InstallProgress {
            server_id,
            progress,
        } => format!(
            "{server_id} install {}",
            if *progress >= 1.0 {
                "completed"
            } else {
                "started"
            }
        ),
        EventBody::DiagnosticsChanged {
            path, server_id, ..
        } => format!("{server_id} diagnostics {}", path.display()),
        EventBody::LeaseChanged { session_id, active } => {
            format!(
                "lease {session_id} {}",
                if *active { "active" } else { "released" }
            )
        }
        EventBody::BrokerMessage { message } => message.clone(),
    }
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.into();
    }
    value
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>()
        + "..."
}

struct RestoreTerminal;

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = ratatui::try_restore();
    }
}

#[cfg(test)]
mod tests {
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
}

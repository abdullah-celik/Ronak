//! A simulated desktop, for developing and testing without GNOME.
//!
//! Plays a scripted morning on a 1920x1200 laptop: some coding (editor,
//! terminal, docs in a browser, a glance at Slack), then a video meeting,
//! then back to coding. It obeys commands the way a real compositor would,
//! moving windows and reporting the new geometry back.
//!
//! `ronakd run --platform mock --auto-accept` shows the whole pipeline end
//! to end on any machine, including the cloud.

use std::time::Duration;

use ronak_core::adapter::{AdapterHandle, AdapterRequest};
use ronak_core::model::*;
use ronak_core::protocol::*;
use tokio::sync::mpsc;
use tracing::info;

#[derive(Debug, Clone)]
pub struct MockOptions {
    /// Accept every suggestion shortly after it is shown.
    pub auto_accept: bool,
    /// Length of one script beat.
    pub beat: Duration,
    /// Play the script once and then stay idle (for tests).
    pub once: bool,
}

const WORK_AREA: Rect = Rect::new(0, 32, 1920, 1168);

fn window(id: &str, app: &str, name: &str, title: &str) -> Window {
    Window {
        id: id.into(),
        app: Some(App { id: app.into(), name: name.into(), category: AppCategory::Other }),
        title: Some(title.into()),
        kind: WindowKind::Normal,
        rect: WORK_AREA,
        monitor: 0,
        workspace: Some(0),
        focused: false,
        minimized: false,
        maximized: true,
        fullscreen: false,
        redacted: false,
        last_focused_at: None,
        pid: None,
    }
}

enum Step {
    Focus(&'static str, u32),
    Open(Window),
    Close(&'static str),
    Pause(u32),
}

fn script() -> Vec<Step> {
    use Step::*;
    vec![
        Focus("10", 6),
        Focus("11", 3),
        Focus("10", 4),
        Focus("12", 3),
        Focus("10", 5),
        Focus("13", 1),
        Focus("10", 6),
        Open(window("14", "us.zoom.Zoom", "Zoom", "Zoom Meeting")),
        Focus("14", 10),
        Focus("15", 2),
        Focus("14", 8),
        Close("14"),
        Focus("10", 8),
        Pause(4),
    ]
}

pub fn spawn(opts: MockOptions) -> AdapterHandle {
    let (event_tx, event_rx) = mpsc::channel(256);
    let (command_tx, command_rx) = mpsc::channel(64);
    tokio::spawn(run(opts, event_tx, command_rx));
    AdapterHandle { name: "mock", events: event_rx, commands: command_tx }
}

struct Desk {
    windows: Vec<Window>,
    session: Session,
    tx: mpsc::Sender<PlatformEvent>,
}

impl Desk {
    async fn emit(&self, event: PlatformEvent) {
        let _ = self.tx.send(event).await;
    }

    async fn execute(&mut self, actions: &[Action]) -> CommandResult {
        let mut errors = vec![];
        for action in actions {
            match action {
                Action::Place { placements } => {
                    for p in placements {
                        let Some(w) = self.windows.iter_mut().find(|w| w.id == p.window_id) else {
                            errors.push(format!("no window {}", p.window_id));
                            continue;
                        };
                        if p.maximized {
                            w.maximized = true;
                            w.rect = WORK_AREA;
                        } else {
                            w.maximized = false;
                            w.rect = p.rect;
                        }
                        let w = w.clone();
                        self.emit(PlatformEvent::WindowChanged { window: w }).await;
                    }
                }
                Action::Focus { window_id } => {
                    self.emit(PlatformEvent::FocusChanged { window_id: Some(window_id.clone()) }).await
                }
                Action::SetDoNotDisturb { enabled } => {
                    self.session.do_not_disturb = *enabled;
                    info!("mock: notifications {}", if *enabled { "paused" } else { "resumed" });
                    self.emit(PlatformEvent::SessionChanged { session: self.session }).await;
                }
                Action::Minimize { window_id } | Action::Unminimize { window_id } => {
                    let min = matches!(action, Action::Minimize { .. });
                    if let Some(w) = self.windows.iter_mut().find(|w| &w.id == window_id) {
                        w.minimized = min;
                        let w = w.clone();
                        self.emit(PlatformEvent::WindowChanged { window: w }).await;
                    }
                }
                Action::MoveToWorkspace { .. } | Action::ActivateWorkspace { .. } | Action::CreateWorkspace => {}
            }
        }
        CommandResult { ok: errors.is_empty(), errors }
    }
}

async fn run(opts: MockOptions, tx: mpsc::Sender<PlatformEvent>, mut commands: mpsc::Receiver<AdapterRequest>) {
    let mut desk = Desk {
        windows: vec![
            window("10", "code", "Code", "daemon.rs - ronak - Visual Studio Code"),
            window("11", "Alacritty", "Alacritty", "cargo test"),
            window("12", "org.mozilla.firefox", "Firefox", "zbus - docs.rs"),
            window("13", "com.slack.Slack", "Slack", "Slack | #ronak"),
            window("15", "md.obsidian.Obsidian", "Obsidian", "Meeting notes"),
        ],
        session: Session::default(),
        tx: tx.clone(),
    };
    desk.emit(PlatformEvent::Hello {
        platform: PlatformInfo {
            os: Os::Other,
            desktop: "mock".into(),
            desktop_version: "1".into(),
            adapter: "ronak-mock".into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
        },
        protocol_version: PROTOCOL_VERSION,
    })
    .await;
    desk.emit(PlatformEvent::Snapshot {
        monitors: vec![Monitor {
            index: 0,
            geometry: Rect::new(0, 0, 1920, 1200),
            work_area: WORK_AREA,
            scale: 1.0,
            primary: true,
        }],
        workspaces: Some(Workspaces { active: 0, count: 1 }),
        windows: desk.windows.clone(),
        focused_window: None,
        idle_ms: 0,
        session: desk.session,
    })
    .await;

    let steps = script();
    let mut i = 0usize;
    let mut wait = Duration::ZERO;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                if i >= steps.len() {
                    if opts.once {
                        wait = Duration::from_secs(3600);
                        continue;
                    }
                    i = 0;
                }
                let beats = match &steps[i] {
                    Step::Focus(id, beats) => {
                        desk.emit(PlatformEvent::FocusChanged { window_id: Some((*id).into()) }).await;
                        *beats
                    }
                    Step::Open(w) => {
                        if !desk.windows.iter().any(|x| x.id == w.id) {
                            desk.windows.push(w.clone());
                        }
                        desk.emit(PlatformEvent::WindowOpened { window: w.clone() }).await;
                        0
                    }
                    Step::Close(id) => {
                        desk.windows.retain(|w| w.id != *id);
                        desk.emit(PlatformEvent::WindowClosed { window_id: (*id).into() }).await;
                        0
                    }
                    Step::Pause(beats) => *beats,
                };
                wait = opts.beat * beats;
                i += 1;
            }
            Some(AdapterRequest { command, reply }) = commands.recv() => {
                let result = match command {
                    AdapterCommand::Execute { actions } => desk.execute(&actions).await,
                    AdapterCommand::ShowSuggestion { suggestion } => {
                        info!("mock: suggestion {} -> {}", suggestion.id, suggestion.summary);
                        if opts.auto_accept {
                            let tx = tx.clone();
                            let delay = opts.beat / 2;
                            tokio::spawn(async move {
                                tokio::time::sleep(delay).await;
                                let _ = tx.send(PlatformEvent::User { action: UserAction::AcceptSuggestion { suggestion_id: suggestion.id } }).await;
                            });
                        }
                        CommandResult::ok()
                    }
                    AdapterCommand::ArrangePanel { visible, style, width } => {
                        info!("mock: panel visible={visible} style={style:?} width={width}");
                        CommandResult::ok()
                    }
                    AdapterCommand::ClearSuggestion | AdapterCommand::Configure { .. } => CommandResult::ok(),
                };
                let _ = reply.send(result);
            }
            else => break,
        }
    }
}

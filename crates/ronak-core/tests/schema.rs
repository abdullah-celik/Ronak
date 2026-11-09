//! The JSON Schemas in `schema/` are the source of truth. These tests make
//! sure the Rust types serialize to JSON that validates against them, at every
//! detail level, and that the checked-in examples stay valid.

use chrono::{DateTime, Duration, Utc};
use jsonschema::{Registry, Validator};
use ronak_core::intent::{HeuristicIntent, IntentModel};
use ronak_core::model::*;
use ronak_core::privacy::{default_exclusions, project};
use ronak_core::protocol::*;
use ronak_core::state::DesktopState;
use serde_json::{Value, json};

const CONTEXT_ID: &str = "https://ronak.dev/schema/context/0.2/context.schema.json";

fn load(name: &str) -> Value {
    let path = format!("{}/../../schema/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn validator(name: &str) -> Validator {
    let registry = Registry::new().add(CONTEXT_ID, load("context.schema.json")).unwrap().prepare().unwrap();
    jsonschema::options().with_registry(&registry).should_validate_formats(true).build(&load(name)).unwrap()
}

fn assert_valid(v: &Validator, instance: &Value) {
    let errors: Vec<String> = v.iter_errors(instance).map(|e| format!("{} at {}", e, e.instance_path())).collect();
    assert!(
        errors.is_empty(),
        "schema errors:\n{}\nfor:\n{}",
        errors.join("\n"),
        serde_json::to_string_pretty(instance).unwrap()
    );
}

fn t(secs: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-09T10:00:00Z").unwrap().with_timezone(&Utc) + Duration::seconds(secs)
}

fn window(id: &str, app: &str, name: &str, title: &str) -> Window {
    Window {
        id: id.into(),
        app: Some(App { id: app.into(), name: name.into(), category: AppCategory::Other }),
        title: Some(title.into()),
        kind: WindowKind::Normal,
        rect: Rect::new(0, 32, 1920, 1168),
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

fn sample_state() -> DesktopState {
    let mut s = DesktopState::new();
    s.apply(
        &PlatformEvent::Hello {
            platform: PlatformInfo {
                os: Os::Linux,
                desktop: "gnome".into(),
                desktop_version: "46.0".into(),
                adapter: "ronak-gnome-extension".into(),
                adapter_version: "0.1.0".into(),
            },
            protocol_version: PROTOCOL_VERSION,
        },
        t(0),
    );
    let windows = vec![
        window("4294967297", "code", "Code", "main.rs - ronak - Visual Studio Code"),
        window("4294967298", "Alacritty", "Alacritty", "cargo test"),
        window("4294967299", "org.mozilla.firefox", "Firefox", "zbus - docs.rs"),
        window("4294967300", "org.keepassxc.KeePassXC", "KeePassXC", "Passwords.kdbx"),
    ]
    .into_iter()
    .map(|w| ronak_core::privacy::apply_exclusions(&default_exclusions(), w, true).unwrap())
    .collect();
    s.apply(
        &PlatformEvent::Snapshot {
            monitors: vec![Monitor {
                index: 0,
                geometry: Rect::new(0, 0, 1920, 1200),
                work_area: Rect::new(0, 32, 1920, 1168),
                scale: 1.25,
                primary: true,
            }],
            workspaces: Some(Workspaces { active: 0, count: 2 }),
            windows,
            focused_window: Some("4294967297".into()),
            idle_ms: 1200,
            session: Session::default(),
        },
        t(0),
    );
    s.apply(&PlatformEvent::FocusChanged { window_id: Some("4294967298".into()) }, t(90));
    s.apply(&PlatformEvent::FocusChanged { window_id: Some("4294967297".into()) }, t(120));
    s
}

/// A schema 0.2 snapshot: refined intent, elements and capabilities.
fn rich_snapshot() -> Snapshot {
    let state = sample_state();
    let task = HeuristicIntent::default().infer(&state, t(200)).unwrap();
    let elements = vec![
        Element {
            id: ":1.42/org/a11y/atspi/accessible/7".into(),
            window_id: Some("4294967298".into()),
            parent_id: None,
            role: "terminal".into(),
            name: Some("Terminal".into()),
            states: vec!["focused".into(), "editable".into()],
            text: Some("$ cargo test\ntest result: FAILED. 30 passed; 1 failed".into()),
        },
        Element {
            id: ":1.42/org/a11y/atspi/accessible/9".into(),
            window_id: Some("4294967298".into()),
            parent_id: Some(":1.42/org/a11y/atspi/accessible/7".into()),
            role: "scroll_bar".into(),
            name: None,
            states: vec![],
            text: None,
        },
    ];
    let capabilities = vec![
        Capability {
            id: "media:spotify".into(),
            kind: CapabilityKind::Media,
            provider: "org.mpris.MediaPlayer2.spotify".into(),
            name: "Spotify".into(),
            access: Access::Act,
            actions: vec!["play_pause".into(), "next".into(), "previous".into()],
            media: Some(MediaState {
                status: PlaybackStatus::Playing,
                title: Some("Weightless".into()),
                artist: Some("Marconi Union".into()),
            }),
            calendar: None,
            notifications: None,
            mcp: None,
        },
        Capability {
            id: "calendar".into(),
            kind: CapabilityKind::Calendar,
            provider: "evolution-data-server".into(),
            name: "Calendar".into(),
            access: Access::Read,
            actions: vec![],
            media: None,
            calendar: Some(CalendarState {
                current: vec![],
                upcoming: vec![CalendarEvent {
                    summary: Some("Design review".into()),
                    start: t(1800),
                    end: t(5400),
                    location: Some("Video call".into()),
                    all_day: false,
                }],
            }),
            notifications: None,
            mcp: None,
        },
        Capability {
            id: "notifications".into(),
            kind: CapabilityKind::Notifications,
            provider: "gnome-shell".into(),
            name: "Notifications".into(),
            access: Access::Read,
            actions: vec![],
            media: None,
            calendar: None,
            notifications: Some(NotificationsState {
                recent: vec![NotificationInfo {
                    app: "Slack".into(),
                    title: Some("Ana".into()),
                    body: Some("Lunch?".into()),
                    at: t(150),
                    urgency: Urgency::Normal,
                }],
                count_last_hour: 1,
            }),
            mcp: None,
        },
    ];
    let signals = ronak_core::activity::Signals { elements: &elements, capabilities: &capabilities };
    let task = ronak_core::activity::refine(task, &state, &signals, t(200));
    let mut snap = state.snapshot(t(200), Some(task));
    snap.detail = DetailLevel::Content;
    snap.elements = elements;
    snap.capabilities = capabilities;
    snap
}

#[test]
fn snapshots_validate_at_every_detail_level() {
    let v = validator("context.schema.json");
    let full = rich_snapshot();
    let task = full.task.as_ref().unwrap();
    assert_eq!(task.label, "coding");
    assert_eq!(task.activity.as_deref(), Some("testing"));
    for level in [DetailLevel::Content, DetailLevel::Full, DetailLevel::TaskAndApps, DetailLevel::TaskOnly] {
        let projected = project(&full, level);
        let value = serde_json::to_value(&projected).unwrap();
        assert_valid(&v, &value);
        let has_titles = projected.windows.iter().any(|w| w.title.is_some());
        assert_eq!(has_titles, level >= DetailLevel::Full);
        let has_text = projected.elements.iter().any(|e| e.text.is_some());
        assert_eq!(has_text, level == DetailLevel::Content);
    }
}

#[test]
fn surfaces_pass_both_validators() {
    use ronak_core::surface::{ComposeInput, Composer, RulesComposer, validate};
    let v = validator("surface.schema.json");
    let snap = rich_snapshot();
    let suggestion = SuggestionView {
        id: "s3".into(),
        task_label: "coding".into(),
        summary: "coding: Code main · Alacritty side".into(),
    };
    let input = ComposeInput {
        task: snap.task.as_ref(),
        capabilities: &snap.capabilities,
        suggestion: Some(&suggestion),
        observing: true,
        undo_available: true,
        accessibility_question: Some(("code", "Code")),
        now: t(200),
    };
    let surfaces = RulesComposer.compose(&input);
    assert_eq!(surfaces.len(), 2);
    for s in &surfaces {
        validate(s).unwrap();
        assert_valid(&v, &serde_json::to_value(s).unwrap());
    }
    // Hand-written examples (what an agent or model might send) validate too.
    let dir = format!("{}/../../schema/examples/surfaces", env!("CARGO_MANIFEST_DIR"));
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_valid(&v, &value);
        ronak_core::surface::parse(&value).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
    }
}

#[test]
fn protocol_messages_validate() {
    let v = validator("adapter-protocol.schema.json");
    let state = sample_state();
    let w = state.windows.values().next().unwrap().clone();
    let events = vec![
        PlatformEvent::WindowOpened { window: w.clone() },
        PlatformEvent::WindowChanged { window: w },
        PlatformEvent::WindowClosed { window_id: "1".into() },
        PlatformEvent::FocusChanged { window_id: None },
        PlatformEvent::WorkspacesChanged { workspaces: Workspaces { active: 1, count: 3 } },
        PlatformEvent::MonitorsChanged { monitors: state.monitors.clone() },
        PlatformEvent::Activity { idle_ms: 5000 },
        PlatformEvent::SessionChanged { session: Session { do_not_disturb: true, observing: true } },
        PlatformEvent::User { action: UserAction::AcceptSuggestion { suggestion_id: "s1".into() } },
        PlatformEvent::User { action: UserAction::Undo },
        PlatformEvent::User { action: UserAction::SetObserving { observing: false } },
        PlatformEvent::User {
            action: UserAction::CorrectIntent {
                label: "writing".into(),
                activity: Some("drafting".into()),
                project: None,
            },
        },
        PlatformEvent::User { action: UserAction::TogglePanel },
        PlatformEvent::Notification {
            notification: NotificationInfo {
                app: "Slack".into(),
                title: Some("Ana".into()),
                body: None,
                at: t(5),
                urgency: Urgency::High,
            },
        },
        PlatformEvent::Gone,
    ];
    for e in &events {
        assert_valid(&v, &serde_json::to_value(e).unwrap());
    }
    assert_valid(&v, &serde_json::to_value(&events).unwrap());

    let commands = vec![
        AdapterCommand::Execute {
            actions: vec![
                Action::Place {
                    placements: vec![Placement {
                        window_id: "1".into(),
                        rect: Rect::new(8, 40, 900, 700),
                        maximized: false,
                    }],
                },
                Action::Focus { window_id: "1".into() },
                Action::MoveToWorkspace { window_id: "1".into(), workspace: 2 },
                Action::ActivateWorkspace { workspace: 0 },
                Action::CreateWorkspace,
                Action::SetDoNotDisturb { enabled: true },
            ],
        },
        AdapterCommand::ShowSuggestion {
            suggestion: SuggestionView {
                id: "s1".into(),
                task_label: "coding".into(),
                summary: "coding: Code main".into(),
            },
        },
        AdapterCommand::ClearSuggestion,
        AdapterCommand::ArrangePanel { visible: true, style: ronak_core::surface::PanelStyle::SidePanel, width: 360 },
        AdapterCommand::Configure {
            config: AdapterConfig {
                exclusions: default_exclusions(),
                observe_titles: true,
                activity_interval_ms: 15000,
                observe_notifications: true,
            },
        },
    ];
    for c in &commands {
        assert_valid(&v, &serde_json::to_value(c).unwrap());
    }
    assert_valid(&v, &serde_json::to_value(CommandResult::error("nope")).unwrap());

    // And a malformed message must be rejected.
    assert!(!v.is_valid(&json!({ "type": "focus_changed", "window": 3 })));
}

#[test]
fn checked_in_examples_validate() {
    let v = validator("context.schema.json");
    let mut seen = 0;
    // 0.1 examples are kept unchanged: 0.2 is additive, so they must stay valid.
    for sub in ["0.1", ""] {
        let dir = format!("{}/../../schema/examples/{sub}", env!("CARGO_MANIFEST_DIR"));
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json") {
                let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_valid(&v, &value);
                // The examples must also round-trip through the Rust types.
                let _: Snapshot = serde_json::from_value(value).unwrap();
                seen += 1;
            }
        }
    }
    assert!(seen >= 6, "expected the 0.1 and 0.2 examples");
}

/// Regenerates the 0.2 examples in `schema/examples/` from the sample state:
/// `cargo test -p ronak-core --test schema -- --ignored write_examples`
#[test]
#[ignore]
fn write_examples() {
    let full = rich_snapshot();
    let dir = format!("{}/../../schema/examples", env!("CARGO_MANIFEST_DIR"));
    for (name, level) in [
        ("snapshot.content.json", DetailLevel::Content),
        ("snapshot.full.json", DetailLevel::Full),
        ("snapshot.task-and-apps.json", DetailLevel::TaskAndApps),
        ("snapshot.task-only.json", DetailLevel::TaskOnly),
    ] {
        let json = serde_json::to_string_pretty(&project(&full, level)).unwrap();
        std::fs::write(format!("{dir}/{name}"), json + "\n").unwrap();
    }

    // The rules composer's surfaces, plus a panel message for `ronak-panel --demo`.
    use ronak_core::surface::{ComposeInput, Composer, RulesComposer, wrap_proposal};
    let suggestion = SuggestionView {
        id: "s3".into(),
        task_label: "coding".into(),
        summary: "coding: Code main · Alacritty side".into(),
    };
    let input = ComposeInput {
        task: full.task.as_ref(),
        capabilities: &full.capabilities,
        suggestion: Some(&suggestion),
        observing: true,
        undo_available: true,
        accessibility_question: None,
        now: t(200),
    };
    let surfaces = RulesComposer.compose(&input);
    for (s, name) in surfaces.iter().zip(["intent-panel.json", "context-glances.json"]) {
        let json = serde_json::to_string_pretty(s).unwrap();
        std::fs::write(format!("{dir}/surfaces/{name}"), json + "\n").unwrap();
    }
    let agent: Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/surfaces/agent-proposal.json")).unwrap()).unwrap();
    let proposal = wrap_proposal(ronak_core::surface::parse(&agent).unwrap(), "p1", "claude-code");
    let mut all = surfaces.clone();
    all.insert(1, proposal);
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "surfaces",
        "params": { "surfaces": all, "panel": { "visible": true, "style": "side_panel", "width": 360 } }
    });
    std::fs::create_dir_all(format!("{dir}/panel")).unwrap();
    std::fs::write(format!("{dir}/panel/demo.json"), serde_json::to_string_pretty(&msg).unwrap() + "\n").unwrap();
}

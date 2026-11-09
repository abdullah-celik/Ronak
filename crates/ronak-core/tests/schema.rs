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

const CONTEXT_ID: &str = "https://ronak.dev/schema/context/0.1/context.schema.json";

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

#[test]
fn snapshots_validate_at_every_detail_level() {
    let v = validator("context.schema.json");
    let state = sample_state();
    let task = HeuristicIntent::default().infer(&state, t(200));
    assert_eq!(task.as_ref().unwrap().label, "coding");
    let full = state.snapshot(t(200), task);
    for level in [DetailLevel::Full, DetailLevel::TaskAndApps, DetailLevel::TaskOnly] {
        let projected = project(&full, level);
        let value = serde_json::to_value(&projected).unwrap();
        assert_valid(&v, &value);
        let has_titles = projected.windows.iter().any(|w| w.title.is_some());
        assert_eq!(has_titles, level == DetailLevel::Full);
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
        AdapterCommand::Configure {
            config: AdapterConfig {
                exclusions: default_exclusions(),
                observe_titles: true,
                activity_interval_ms: 15000,
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
    let dir = format!("{}/../../schema/examples", env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
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
    assert!(seen >= 1, "expected at least one example");
}

/// Regenerates `schema/examples/` from the sample state:
/// `cargo test -p ronak-core --test schema -- --ignored write_examples`
#[test]
#[ignore]
fn write_examples() {
    let state = sample_state();
    let task = HeuristicIntent::default().infer(&state, t(200));
    let full = state.snapshot(t(200), task);
    let dir = format!("{}/../../schema/examples", env!("CARGO_MANIFEST_DIR"));
    for (name, level) in [
        ("snapshot.full.json", DetailLevel::Full),
        ("snapshot.task-and-apps.json", DetailLevel::TaskAndApps),
        ("snapshot.task-only.json", DetailLevel::TaskOnly),
    ] {
        let json = serde_json::to_string_pretty(&project(&full, level)).unwrap();
        std::fs::write(format!("{dir}/{name}"), json + "\n").unwrap();
    }
}

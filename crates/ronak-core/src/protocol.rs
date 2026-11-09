//! Messages exchanged between the daemon and platform adapters.
//!
//! Formally specified in `schema/adapter-protocol.schema.json` so adapters can
//! be written in any language (the GNOME one is JavaScript inside
//! gnome-shell). Events flow from the adapter to the daemon; commands flow the
//! other way.

use serde::{Deserialize, Serialize};

use crate::model::{Monitor, PlatformInfo, Rect, Session, Window, Workspaces};
use crate::privacy::ExclusionRule;

/// Bumped when the adapter protocol changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// Something that happened on the desktop, reported by an adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlatformEvent {
    /// First message after connecting.
    Hello {
        platform: PlatformInfo,
        protocol_version: u32,
    },
    /// Full state. Sent after `hello` and whenever the adapter resyncs.
    Snapshot {
        monitors: Vec<Monitor>,
        #[serde(default)]
        workspaces: Option<Workspaces>,
        windows: Vec<Window>,
        #[serde(default)]
        focused_window: Option<String>,
        #[serde(default)]
        idle_ms: u64,
        #[serde(default)]
        session: Session,
    },
    WindowOpened {
        window: Window,
    },
    /// The full new state of a window whose title, geometry or flags changed.
    WindowChanged {
        window: Window,
    },
    WindowClosed {
        window_id: String,
    },
    FocusChanged {
        #[serde(default)]
        window_id: Option<String>,
    },
    WorkspacesChanged {
        workspaces: Workspaces,
    },
    MonitorsChanged {
        monitors: Vec<Monitor>,
    },
    /// Periodic input-activity sample. Contains timing only, never keys.
    Activity {
        idle_ms: u64,
    },
    SessionChanged {
        session: Session,
    },
    /// The user did something through the adapter's UI (keybinding, menu).
    User {
        action: UserAction,
    },
    /// The adapter disconnected. Synthesized by the daemon side.
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserAction {
    AcceptSuggestion { suggestion_id: String },
    DismissSuggestion { suggestion_id: String },
    Undo,
    SetObserving { observing: bool },
}

/// Where one window should go. `maximized: true` means "maximize it" and
/// `rect` is then ignored (used when undoing back to a maximized window).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub window_id: String,
    pub rect: Rect,
    #[serde(default)]
    pub maximized: bool,
}

/// Something Ronak can change on the desktop. Every action is reversible
/// through [`crate::actions::inverse`] except where noted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    Place {
        placements: Vec<Placement>,
    },
    Focus {
        window_id: String,
    },
    Minimize {
        window_id: String,
    },
    Unminimize {
        window_id: String,
    },
    MoveToWorkspace {
        window_id: String,
        workspace: u32,
    },
    ActivateWorkspace {
        workspace: u32,
    },
    /// Not reversible: desktops remove empty workspaces on their own.
    CreateWorkspace,
    SetDoNotDisturb {
        enabled: bool,
    },
}

/// A suggestion as the adapter's UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionView {
    pub id: String,
    pub task_label: String,
    pub summary: String,
}

/// Settings the daemon pushes to the adapter so privacy rules are applied
/// before data ever leaves the compositor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterConfig {
    pub exclusions: Vec<ExclusionRule>,
    pub observe_titles: bool,
    /// How often to sample input activity, in milliseconds.
    pub activity_interval_ms: u64,
}

/// A request from the daemon to the adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum AdapterCommand {
    Execute { actions: Vec<Action> },
    ShowSuggestion { suggestion: SuggestionView },
    ClearSuggestion,
    Configure { config: AdapterConfig },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommandResult {
    pub ok: bool,
    #[serde(default)]
    pub errors: Vec<String>,
}

impl CommandResult {
    pub fn ok() -> Self {
        Self { ok: true, errors: vec![] }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self { ok: false, errors: vec![msg.into()] }
    }
}

//! The context model: Rust mirror of `schema/context.schema.json`.
//!
//! The JSON Schema is the source of truth. These types must serialize to JSON
//! that validates against it; `tests/schema.rs` enforces that. If you change a
//! field here, change the schema first and bump [`SCHEMA_VERSION`] when the
//! change is not backwards compatible.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Version of the context schema these types implement.
pub const SCHEMA_VERSION: &str = "0.1";

/// Well-known task labels. The vocabulary is open: models may emit others,
/// but these are the ones policies and layouts understand today.
pub const KNOWN_TASKS: &[&str] =
    &["coding", "writing", "reading", "research", "communication", "meeting", "design", "media", "admin"];

/// A rectangle in logical (scaled) desktop pixels. Origin is the top-left of
/// the whole desktop, so on multi-monitor setups `x` can exceed one screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub const fn area(&self) -> i64 {
        self.w as i64 * self.h as i64
    }
    /// True when the two rectangles share any interior area.
    pub const fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    /// True when `o` lies entirely inside `self`.
    pub const fn contains(&self, o: &Rect) -> bool {
        o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()
    }
    /// Shrinks the rectangle by `px` on every side.
    pub fn inset(&self, px: i32) -> Rect {
        Rect::new(self.x + px, self.y + px, (self.w - 2 * px).max(0), (self.h - 2 * px).max(0))
    }
    /// Largest distance any edge moved between `self` and `o`. Used to decide
    /// whether a change is big enough to be worth moving a window.
    pub fn max_edge_delta(&self, o: &Rect) -> i32 {
        [
            (self.x - o.x).abs(),
            (self.y - o.y).abs(),
            (self.right() - o.right()).abs(),
            (self.bottom() - o.bottom()).abs(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
    }
}

/// How much detail a consumer may see. Ordered from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailLevel {
    /// Only the task label and confidence. No windows, no apps.
    TaskOnly,
    /// Task plus which apps are open and their roles. No window titles.
    TaskAndApps,
    /// Everything, including window titles (still after exclusion rules).
    Full,
}

impl DetailLevel {
    pub fn allows_windows(self) -> bool {
        self >= DetailLevel::TaskAndApps
    }
    pub fn allows_titles(self) -> bool {
        self == DetailLevel::Full
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Linux,
    Windows,
    Macos,
    Other,
}

/// Which desktop produced the data, and through which adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub os: Os,
    /// e.g. "gnome", "kde", "hyprland", "windows-shell", "mock".
    pub desktop: String,
    pub desktop_version: String,
    /// e.g. "ronak-gnome-extension".
    pub adapter: String,
    pub adapter_version: String,
}

impl PlatformInfo {
    pub fn unknown() -> Self {
        Self {
            os: Os::Other,
            desktop: "unknown".into(),
            desktop_version: String::new(),
            adapter: "none".into(),
            adapter_version: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub index: u32,
    /// Full monitor area.
    pub geometry: Rect,
    /// Area usable by windows (excludes panels and docks).
    pub work_area: Rect,
    /// Scale factor, e.g. 1.0, 1.25, 2.0.
    pub scale: f64,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspaces {
    /// Zero-based index of the active workspace.
    pub active: u32,
    pub count: u32,
}

/// Broad application category. Filled in by [`crate::apps::categorize`];
/// adapters may leave it as `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppCategory {
    Editor,
    Terminal,
    Browser,
    Communication,
    Mail,
    Meeting,
    /// Writing and office apps: Writer, Obsidian, Typora, spreadsheets.
    Documents,
    /// Viewers: PDF readers, e-book readers.
    Reader,
    Media,
    Design,
    Files,
    System,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    /// Stable application id: a desktop-file id without ".desktop" on Linux
    /// (e.g. "org.gnome.Nautilus"), an executable name on Windows.
    pub id: String,
    /// Human-readable name, e.g. "Files".
    pub name: String,
    #[serde(default)]
    pub category: AppCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    #[default]
    Normal,
    Dialog,
    Utility,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Opaque id, stable for the lifetime of the window.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<App>,
    /// Absent when redacted by an exclusion rule or a detail level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: WindowKind,
    pub rect: Rect,
    #[serde(default)]
    pub monitor: u32,
    /// `None` means the window is visible on all workspaces.
    #[serde(default)]
    pub workspace: Option<u32>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub minimized: bool,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub fullscreen: bool,
    /// True when an exclusion rule removed sensitive fields.
    #[serde(default)]
    pub redacted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_focused_at: Option<DateTime<Utc>>,
}

impl Window {
    pub fn app_id(&self) -> Option<&str> {
        self.app.as_ref().map(|a| a.id.as_str())
    }
    pub fn category(&self) -> AppCategory {
        self.app.as_ref().map(|a| a.category).unwrap_or_default()
    }
    pub fn app_name(&self) -> &str {
        self.app.as_ref().map(|a| a.name.as_str()).unwrap_or("unknown app")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    /// Input within the last minute.
    Active,
    /// No input for 1 to 5 minutes.
    Idle,
    /// No input for more than 5 minutes.
    Away,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub state: ActivityState,
    /// Milliseconds since the last keyboard or pointer input. Never which keys.
    pub idle_ms: u64,
    /// Focus switches between windows in the last five minutes.
    pub switches_last_5min: u32,
}

/// Session-wide switches that policies care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub do_not_disturb: bool,
    /// False while the user has paused observation.
    pub observing: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self { do_not_disturb: false, observing: true }
    }
}

/// What a window is doing for the current task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Where the work happens: gets the most space.
    Primary,
    /// Consulted while working: docs, terminal output, a spec.
    Reference,
    /// Glanced at occasionally: chat, music, a build log.
    Monitor,
    /// Not part of the task right now.
    Background,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskMember {
    pub window_id: String,
    pub role: Role,
    /// Relative importance within the task, 0.0 to 1.0.
    pub weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    Heuristic,
    Classifier,
    Llm,
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// See [`KNOWN_TASKS`]; the vocabulary is open.
    pub label: String,
    pub confidence: f32,
    pub source: TaskSource,
    pub since: DateTime<Utc>,
    #[serde(default)]
    pub members: Vec<TaskMember>,
}

impl Task {
    pub fn member(&self, window_id: &str) -> Option<&TaskMember> {
        self.members.iter().find(|m| m.window_id == window_id)
    }
}

/// One moment of desktop context. This is the core object of the open schema:
/// adapters produce the raw parts, the context engine adds `task`, and
/// [`crate::privacy::project`] reduces it to what a consumer may see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: String,
    pub captured_at: DateTime<Utc>,
    pub detail: DetailLevel,
    pub platform: PlatformInfo,
    pub monitors: Vec<Monitor>,
    #[serde(default)]
    pub workspaces: Option<Workspaces>,
    pub windows: Vec<Window>,
    #[serde(default)]
    pub focused_window: Option<String>,
    pub activity: Activity,
    pub session: Session,
    #[serde(default)]
    pub task: Option<Task>,
}

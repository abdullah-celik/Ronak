//! The live desktop model.
//!
//! Adapters report events; [`DesktopState::apply`] folds them into a model of
//! the desktop and returns what changed, so the daemon can decide whether to
//! store something or re-run the intent engine. The state also keeps a short
//! focus history: which window had focus, for how long. That history is the
//! main evidence the intent engine uses.

use std::collections::{BTreeMap, HashMap, VecDeque};

use chrono::{DateTime, Duration, Utc};

use crate::apps;
use crate::model::*;
use crate::protocol::{PlatformEvent, UserAction};

/// How much focus history is kept in memory.
const FOCUS_HISTORY: Duration = Duration::minutes(30);
const SWITCH_WINDOW: Duration = Duration::minutes(5);

/// A continuous period during which one window had focus.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusSpan {
    pub window_id: String,
    pub app_id: Option<String>,
    pub category: AppCategory,
    /// The title looked like developer work (docs, GitHub, localhost).
    pub dev_hint: bool,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl FocusSpan {
    pub fn seconds(&self) -> f64 {
        (self.end - self.start).num_milliseconds().max(0) as f64 / 1000.0
    }
}

/// What an event changed.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Nothing,
    Hello,
    /// The adapter sent a full snapshot.
    Resync,
    /// A window opened, closed, or changed title or flags.
    Windows,
    /// A window moved or resized.
    Geometry {
        window_id: String,
    },
    /// Focus moved. `closed` is the span that just ended, if any.
    Focus {
        closed: Option<FocusSpan>,
    },
    Workspaces,
    Monitors,
    Activity,
    Session,
    User(UserAction),
    Gone,
}

#[derive(Debug, Default)]
pub struct DesktopState {
    pub platform: Option<PlatformInfo>,
    pub monitors: Vec<Monitor>,
    pub workspaces: Option<Workspaces>,
    /// Ordered by id so iteration (and therefore every decision) is deterministic.
    pub windows: BTreeMap<String, Window>,
    pub focused: Option<String>,
    pub idle_ms: u64,
    pub session: Session,
    open_focus: Option<FocusSpan>,
    focus_log: VecDeque<FocusSpan>,
    switches: VecDeque<DateTime<Utc>>,
    geometry_changed_at: HashMap<String, DateTime<Utc>>,
}

impl DesktopState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_connected(&self) -> bool {
        self.platform.is_some()
    }

    /// Folds one event into the model.
    pub fn apply(&mut self, event: &PlatformEvent, now: DateTime<Utc>) -> Change {
        match event {
            PlatformEvent::Hello { platform, .. } => {
                self.platform = Some(platform.clone());
                Change::Hello
            }
            PlatformEvent::Snapshot { monitors, workspaces, windows, focused_window, idle_ms, session } => {
                self.monitors = monitors.clone();
                self.workspaces = *workspaces;
                let old = std::mem::take(&mut self.windows);
                self.windows = windows
                    .iter()
                    .cloned()
                    .map(|w| {
                        let mut w = normalize(w);
                        w.last_focused_at = w.last_focused_at.or(old.get(&w.id).and_then(|o| o.last_focused_at));
                        (w.id.clone(), w)
                    })
                    .collect();
                self.idle_ms = *idle_ms;
                self.session = *session;
                // Keep the open focus span if the same window is still focused.
                if self.focused.as_deref() != focused_window.as_deref() {
                    let _ = self.set_focus(focused_window.clone(), now);
                } else if let Some(id) = focused_window
                    && let Some(w) = self.windows.get_mut(id)
                {
                    w.last_focused_at = Some(now);
                }
                Change::Resync
            }
            PlatformEvent::WindowOpened { window } => {
                self.windows.insert(window.id.clone(), normalize(window.clone()));
                Change::Windows
            }
            PlatformEvent::WindowChanged { window } => {
                let new = normalize(window.clone());
                let moved = match self.windows.get(&window.id) {
                    Some(old) => old.rect != new.rect || old.maximized != new.maximized,
                    None => false,
                };
                let last_focused_at = self.windows.get(&window.id).and_then(|w| w.last_focused_at);
                let mut new = new;
                new.last_focused_at = new.last_focused_at.or(last_focused_at);
                self.windows.insert(window.id.clone(), new);
                if moved {
                    self.geometry_changed_at.insert(window.id.clone(), now);
                    Change::Geometry { window_id: window.id.clone() }
                } else {
                    Change::Windows
                }
            }
            PlatformEvent::WindowClosed { window_id } => {
                self.windows.remove(window_id);
                self.geometry_changed_at.remove(window_id);
                if self.focused.as_deref() == Some(window_id) {
                    let closed = self.set_focus(None, now);
                    return Change::Focus { closed };
                }
                Change::Windows
            }
            PlatformEvent::FocusChanged { window_id } => {
                let closed = self.set_focus(window_id.clone(), now);
                Change::Focus { closed }
            }
            PlatformEvent::WorkspacesChanged { workspaces } => {
                self.workspaces = Some(*workspaces);
                Change::Workspaces
            }
            PlatformEvent::MonitorsChanged { monitors } => {
                self.monitors = monitors.clone();
                Change::Monitors
            }
            PlatformEvent::Activity { idle_ms } => {
                self.idle_ms = *idle_ms;
                Change::Activity
            }
            PlatformEvent::SessionChanged { session } => {
                self.session = *session;
                Change::Session
            }
            PlatformEvent::User { action } => {
                if let UserAction::SetObserving { observing } = action {
                    self.session.observing = *observing;
                }
                Change::User(action.clone())
            }
            PlatformEvent::Gone => {
                self.platform = None;
                let _ = self.set_focus(None, now);
                Change::Gone
            }
        }
    }

    /// Moves focus and returns the span that just closed. Also records it in
    /// the in-memory focus log.
    fn set_focus(&mut self, new: Option<String>, now: DateTime<Utc>) -> Option<FocusSpan> {
        if self.focused == new {
            return None;
        }
        let closed = self.open_focus.take().map(|mut span| {
            span.end = now;
            span
        });
        if let Some(span) = &closed {
            self.focus_log.push_back(span.clone());
            if new.is_some() {
                self.switches.push_back(now);
            }
        }
        if let Some(id) = &new
            && let Some(w) = self.windows.get_mut(id)
        {
            w.last_focused_at = Some(now);
            self.open_focus = Some(FocusSpan {
                window_id: id.clone(),
                app_id: w.app_id().map(str::to_owned),
                category: w.category(),
                dev_hint: apps::looks_like_dev(w.title.as_deref()),
                start: now,
                end: now,
            });
        }
        self.focused = new;
        self.prune(now);
        closed
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        while self.focus_log.front().is_some_and(|s| now - s.end > FOCUS_HISTORY) {
            self.focus_log.pop_front();
        }
        while self.switches.front().is_some_and(|t| now - *t > SWITCH_WINDOW) {
            self.switches.pop_front();
        }
    }

    /// Focus spans overlapping `[now - within, now]`, clipped to that range,
    /// including the currently open span.
    pub fn focus_spans(&self, within: Duration, now: DateTime<Utc>) -> Vec<FocusSpan> {
        let since = now - within;
        let open = self.open_focus.clone().map(|mut s| {
            s.end = now;
            s
        });
        self.focus_log
            .iter()
            .cloned()
            .chain(open)
            .filter(|s| s.end > since)
            .map(|mut s| {
                if s.start < since {
                    s.start = since;
                }
                s
            })
            .collect()
    }

    pub fn activity(&self, now: DateTime<Utc>) -> Activity {
        let state = match self.idle_ms {
            ms if ms >= 300_000 => ActivityState::Away,
            ms if ms >= 60_000 => ActivityState::Idle,
            _ => ActivityState::Active,
        };
        let switches = self.switches.iter().filter(|t| now - **t <= SWITCH_WINDOW).count() as u32;
        Activity { state, idle_ms: self.idle_ms, switches_last_5min: switches }
    }

    pub fn focused_window(&self) -> Option<&Window> {
        self.focused.as_ref().and_then(|id| self.windows.get(id))
    }

    /// The monitor the user is working on: the focused window's, else the primary.
    pub fn active_monitor(&self) -> Option<&Monitor> {
        let idx = self.focused_window().map(|w| w.monitor);
        idx.and_then(|i| self.monitors.iter().find(|m| m.index == i))
            .or_else(|| self.monitors.iter().find(|m| m.primary))
            .or_else(|| self.monitors.first())
    }

    /// Whether a window moved or resized within `within` of `now`, which
    /// usually means the user is dragging it.
    pub fn recently_moved(&self, window_id: &str, within: Duration, now: DateTime<Utc>) -> bool {
        self.geometry_changed_at.get(window_id).is_some_and(|t| now - *t <= within)
    }

    /// True if the window is on the active workspace (or on all of them).
    pub fn on_active_workspace(&self, w: &Window) -> bool {
        match (w.workspace, self.workspaces) {
            (None, _) | (_, None) => true,
            (Some(ws), Some(all)) => ws == all.active,
        }
    }

    /// A full-detail snapshot of the current state, with an optional task.
    pub fn snapshot(&self, now: DateTime<Utc>, task: Option<Task>) -> Snapshot {
        let windows = self
            .windows
            .values()
            .cloned()
            .map(|mut w| {
                w.focused = self.focused.as_deref() == Some(w.id.as_str());
                w
            })
            .collect();
        Snapshot {
            schema_version: SCHEMA_VERSION.into(),
            captured_at: now,
            detail: DetailLevel::Full,
            platform: self.platform.clone().unwrap_or_else(PlatformInfo::unknown),
            monitors: self.monitors.clone(),
            workspaces: self.workspaces,
            windows,
            focused_window: self.focused.clone(),
            activity: self.activity(now),
            session: self.session,
            task,
        }
    }
}

/// Fills in what adapters may leave out, such as the app category.
fn normalize(mut w: Window) -> Window {
    if let Some(app) = &mut w.app
        && app.category == AppCategory::Other
    {
        app.category = apps::categorize(&app.id, w.title.as_deref());
    }
    w
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn win(id: &str, app: &str, title: &str, rect: Rect) -> Window {
        Window {
            id: id.into(),
            app: Some(App { id: app.into(), name: app.into(), category: AppCategory::Other }),
            title: Some(title.into()),
            kind: WindowKind::Normal,
            rect,
            monitor: 0,
            workspace: Some(0),
            focused: false,
            minimized: false,
            maximized: false,
            fullscreen: false,
            redacted: false,
            last_focused_at: None,
        }
    }

    #[test]
    fn focus_spans_accumulate() {
        let t0 = DateTime::parse_from_rfc3339("2026-11-09T10:00:00Z").unwrap().with_timezone(&Utc);
        let mut s = DesktopState::new();
        s.apply(&PlatformEvent::WindowOpened { window: win("1", "code", "main.rs", Rect::new(0, 0, 100, 100)) }, t0);
        s.apply(&PlatformEvent::WindowOpened { window: win("2", "Alacritty", "zsh", Rect::new(0, 0, 100, 100)) }, t0);
        s.apply(&PlatformEvent::FocusChanged { window_id: Some("1".into()) }, t0);
        let c = s.apply(&PlatformEvent::FocusChanged { window_id: Some("2".into()) }, t0 + Duration::seconds(30));
        match c {
            Change::Focus { closed: Some(span) } => {
                assert_eq!(span.window_id, "1");
                assert_eq!(span.category, AppCategory::Editor);
                assert_eq!(span.seconds(), 30.0);
            }
            other => panic!("unexpected {other:?}"),
        }
        let spans = s.focus_spans(Duration::minutes(10), t0 + Duration::seconds(40));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].seconds(), 10.0);
        assert_eq!(s.activity(t0 + Duration::seconds(40)).switches_last_5min, 1);
    }

    #[test]
    fn geometry_changes_are_detected() {
        let t0 = Utc::now();
        let mut s = DesktopState::new();
        s.apply(&PlatformEvent::WindowOpened { window: win("1", "code", "a", Rect::new(0, 0, 100, 100)) }, t0);
        let c = s.apply(&PlatformEvent::WindowChanged { window: win("1", "code", "b", Rect::new(0, 0, 100, 100)) }, t0);
        assert_eq!(c, Change::Windows);
        let c = s.apply(&PlatformEvent::WindowChanged { window: win("1", "code", "b", Rect::new(5, 0, 100, 100)) }, t0);
        assert_eq!(c, Change::Geometry { window_id: "1".into() });
        assert!(s.recently_moved("1", Duration::seconds(3), t0 + Duration::seconds(1)));
    }
}

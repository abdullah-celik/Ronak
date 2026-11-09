//! # ronak-atspi
//!
//! Reads the inside of apps through **AT-SPI**, the accessibility bus that
//! screen readers use. Apps publish a tree of objects (the window, its tabs,
//! text fields, terminals, buttons) with a role, a name, a set of states and,
//! for text-bearing objects, their text.
//!
//! How a read works:
//! 1. Connect to the accessibility bus (a separate D-Bus bus whose address
//!    comes from `org.a11y.Bus` on the session bus).
//! 2. List the applications registered with the AT-SPI registry and find the
//!    one whose process id matches the focused window's.
//! 3. Walk its tree, depth first, within node, depth and time limits. Hidden
//!    subtrees (not "showing") are skipped.
//! 4. Keep only the elements that tell us something: the focused element and
//!    its ancestors, documents, terminals, headings, selected tabs, editable
//!    fields. Read text only where useful, and **never from password fields**.
//!
//! Everything goes through zbus proxies with property caching turned off,
//! because caching subscribes to change signals for every object visited.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use atspi::proxy::accessible::AccessibleProxy;
use atspi::proxy::text::TextProxy;
use atspi::{AccessibilityConnection, Interface, ObjectRefOwned, Role, State};
use ronak_core::model::Element;
use tracing::debug;
use zbus::proxy::CacheProperties;

pub use atspi::connection::{read_session_accessibility, set_session_accessibility};

#[derive(Debug, thiserror::Error)]
pub enum AtspiError {
    #[error("cannot reach the accessibility bus: {0}")]
    Connect(String),
    #[error("accessibility call failed: {0}")]
    Call(String),
}

impl From<zbus::Error> for AtspiError {
    fn from(e: zbus::Error) -> Self {
        AtspiError::Call(e.to_string())
    }
}

impl From<atspi::AtspiError> for AtspiError {
    fn from(e: atspi::AtspiError) -> Self {
        AtspiError::Call(e.to_string())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReadOptions {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_text_chars: usize,
    pub read_content: bool,
    /// Give up on an unresponsive app after this long.
    pub time_budget: Duration,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            max_nodes: 400,
            max_depth: 30,
            max_text_chars: 2000,
            read_content: true,
            time_budget: Duration::from_millis(1500),
        }
    }
}

/// The result of reading one app.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppReading {
    /// False when no accessible app matches the window's process: the app
    /// does not expose its insides (common for Chromium and Electron apps
    /// until accessibility is switched on).
    pub exposes: bool,
    pub elements: Vec<Element>,
    /// The walk stopped at a limit; the reading is partial.
    pub truncated: bool,
    pub nodes_visited: usize,
}

/// Roles worth keeping even when nothing about them is special.
const KEEP_ROLES: &[Role] = &[
    Role::Frame,
    Role::Window,
    Role::Dialog,
    Role::Terminal,
    Role::DocumentWeb,
    Role::DocumentText,
    Role::DocumentFrame,
    Role::DocumentSpreadsheet,
    Role::DocumentPresentation,
    Role::DocumentEmail,
    Role::Heading,
];

/// Roles whose text is worth reading when they have focus (what you type or
/// read right now). Terminals are always read: their output says a lot.
const TEXT_ROLES: &[Role] =
    &[Role::Text, Role::Entry, Role::Paragraph, Role::Terminal, Role::DocumentText, Role::Editbar];

/// "push button" -> "push_button"; "read-only" -> "read_only".
fn snake(s: &str) -> String {
    s.chars().map(|c| if c == ' ' || c == '-' { '_' } else { c.to_ascii_lowercase() }).collect()
}

pub struct AtspiSensor {
    conn: AccessibilityConnection,
    /// Unique bus name -> process id, filled lazily.
    pids: Mutex<HashMap<String, u32>>,
}

impl AtspiSensor {
    pub async fn connect() -> Result<Self, AtspiError> {
        let conn = AccessibilityConnection::new().await.map_err(|e| AtspiError::Connect(e.to_string()))?;
        Ok(Self { conn, pids: Mutex::new(HashMap::new()) })
    }

    fn bus(&self) -> &zbus::Connection {
        self.conn.connection()
    }

    async fn proxy(&self, obj: &ObjectRefOwned) -> Result<AccessibleProxy<'static>, AtspiError> {
        let name = obj.name().ok_or_else(|| AtspiError::Call("object without bus name".into()))?.to_owned();
        Ok(AccessibleProxy::builder(self.bus())
            .destination(name)?
            .path(obj.path().to_owned())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?)
    }

    async fn pid_of(&self, bus_name: &str) -> Option<u32> {
        if let Some(pid) = self.pids.lock().unwrap().get(bus_name) {
            return Some(*pid);
        }
        let dbus = zbus::fdo::DBusProxy::new(self.bus()).await.ok()?;
        let name = zbus::names::BusName::try_from(bus_name.to_string()).ok()?;
        let pid = dbus.get_connection_unix_process_id(name).await.ok()?;
        self.pids.lock().unwrap().insert(bus_name.to_string(), pid);
        Some(pid)
    }

    /// Applications registered on the accessibility bus, with their process ids.
    pub async fn apps(&self) -> Result<Vec<(ObjectRefOwned, Option<u32>, String)>, AtspiError> {
        let root = AccessibleProxy::builder(self.bus())
            .destination("org.a11y.atspi.Registry")?
            .path("/org/a11y/atspi/accessible/root")?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let mut out = vec![];
        for app in root.get_children().await? {
            let Some(name) = app.name().map(|n| n.to_string()) else { continue };
            let pid = self.pid_of(&name).await;
            let label = match self.proxy(&app).await {
                Ok(p) => p.name().await.unwrap_or_default(),
                Err(_) => String::new(),
            };
            out.push((app, pid, label));
        }
        Ok(out)
    }

    /// Reads the app running as `pid`, preferring the top-level window whose
    /// name matches `window_title`, and tags elements with `window_id`.
    pub async fn read_pid(
        &self,
        pid: u32,
        window_title: Option<&str>,
        window_id: Option<&str>,
        opts: &ReadOptions,
    ) -> Result<AppReading, AtspiError> {
        let deadline = Instant::now() + opts.time_budget;
        let apps = self.apps().await?;
        let Some((app, _, _)) = apps.into_iter().find(|(_, p, _)| *p == Some(pid)) else {
            return Ok(AppReading::default());
        };
        let app_proxy = self.proxy(&app).await?;
        let windows = app_proxy.get_children().await?;

        // Pick the window: active, or the one named like the window title.
        let mut chosen = None;
        for w in &windows {
            let Ok(p) = self.proxy(w).await else { continue };
            let states = p.get_state().await.unwrap_or_default();
            let name = p.name().await.unwrap_or_default();
            if states.contains(State::Active) || window_title.is_some_and(|t| !name.is_empty() && t.contains(&name)) {
                chosen = Some(w.clone());
                break;
            }
            if chosen.is_none() && states.contains(State::Showing) {
                chosen = Some(w.clone());
            }
        }
        let Some(top) = chosen.or_else(|| windows.first().cloned()) else {
            return Ok(AppReading { exposes: true, ..Default::default() });
        };

        let mut reading = AppReading { exposes: true, ..Default::default() };
        // Depth-first walk. Each entry: object, depth, nearest kept ancestor id.
        let mut stack: Vec<(ObjectRefOwned, usize, Option<String>)> = vec![(top, 0, None)];
        let mut focused_path: Vec<String> = vec![];
        // The document, in case nothing reports focus (see below).
        let mut document: Option<(usize, ObjectRefOwned)> = None;
        while let Some((obj, depth, kept_parent)) = stack.pop() {
            if reading.nodes_visited >= opts.max_nodes || Instant::now() > deadline {
                reading.truncated = true;
                break;
            }
            reading.nodes_visited += 1;
            let Ok(p) = self.proxy(&obj).await else { continue };
            let role = p.get_role().await.unwrap_or(Role::Invalid);
            let states = p.get_state().await.unwrap_or_default();
            if depth > 0 && !states.contains(State::Showing) {
                continue; // hidden subtree
            }
            let id = format!("{}{}", obj.name().map(|n| n.as_str()).unwrap_or(""), obj.path().as_str());
            let focused = states.contains(State::Focused);
            let selected_tab = role == Role::PageTab && states.contains(State::Selected);
            let editable = states.contains(State::Editable);
            let keep = depth == 0
                || focused
                || selected_tab
                || KEEP_ROLES.contains(&role)
                || (editable && role != Role::PasswordText);

            let mut next_parent = kept_parent.clone();
            if keep {
                let name = p.name().await.ok().filter(|n| !n.is_empty());
                let text = if opts.read_content
                    && role != Role::PasswordText
                    && (role == Role::Terminal || (focused && TEXT_ROLES.contains(&role)))
                {
                    self.read_text(&obj, role == Role::Terminal, opts.max_text_chars).await
                } else {
                    None
                };
                let element = Element {
                    id: id.clone(),
                    window_id: window_id.map(str::to_owned),
                    parent_id: kept_parent.clone(),
                    role: snake(role.name()),
                    name: if role == Role::PasswordText { None } else { name },
                    states: states
                        .iter()
                        .map(|s| snake(s.to_static_str()))
                        .filter(|s| INTERESTING_STATES.contains(&s.as_str()))
                        .collect(),
                    text,
                };
                if focused {
                    focused_path.push(id.clone());
                }
                if document.is_none()
                    && editable
                    && states.contains(State::MultiLine)
                    && TEXT_ROLES.contains(&role)
                    && role != Role::PasswordText
                    && element.text.is_none()
                {
                    document = Some((reading.elements.len(), obj.clone()));
                }
                reading.elements.push(element);
                next_parent = Some(id);
            }
            if depth < opts.max_depth && role != Role::PasswordText {
                if let Ok(children) = p.get_children().await {
                    // Reverse so the first child is visited first.
                    for c in children.into_iter().rev() {
                        stack.push((c, depth + 1, next_parent.clone()));
                    }
                }
            } else if depth >= opts.max_depth {
                reading.truncated = true;
            }
        }
        // Some apps never report focus, and a window without keyboard focus
        // has none. Then the first multi-line editable text is the document
        // being worked on; read that one.
        if focused_path.is_empty()
            && opts.read_content
            && let Some((i, obj)) = document
        {
            reading.elements[i].text = self.read_text(&obj, false, opts.max_text_chars).await;
        }
        debug!(
            pid,
            nodes = reading.nodes_visited,
            kept = reading.elements.len(),
            truncated = reading.truncated,
            "read app"
        );
        Ok(reading)
    }

    /// Reads up to `max` characters: the end for terminals, the start otherwise.
    async fn read_text(&self, obj: &ObjectRefOwned, tail: bool, max: usize) -> Option<String> {
        let a = self.proxy(obj).await.ok()?;
        if !a.get_interfaces().await.ok()?.contains(Interface::Text) {
            return None;
        }
        let name = obj.name()?.to_owned();
        let t = TextProxy::builder(self.bus())
            .destination(name)
            .ok()?
            .path(obj.path().to_owned())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .ok()?;
        let count = t.character_count().await.ok()?.max(0);
        let max = max as i32;
        let (start, end) = if tail { ((count - max).max(0), count) } else { (0, count.min(max)) };
        let text = t.get_text(start, end).await.ok()?;
        // Embedded objects show up as U+FFFC; trailing whitespace is noise.
        let text: String = text.replace('\u{FFFC}', "").trim_end().to_string();
        (!text.trim().is_empty()).then_some(text)
    }
}

/// States that matter for intent; the rest are dropped to keep readings small.
const INTERESTING_STATES: &[&str] =
    &["active", "focused", "selected", "editable", "read_only", "checked", "expanded", "multi_line", "visited", "busy"];

/// Finds the focused element in a reading, if any.
pub fn focused(reading: &AppReading) -> Option<&Element> {
    reading.elements.iter().rev().find(|e| e.has_state("focused"))
}

/// Apps known to hide their accessibility tree until it is switched on.
pub fn needs_accessibility_switch(app_id: &str) -> bool {
    let id = app_id.to_lowercase();
    [
        "chrome", "chromium", "electron", "code", "cursor", "slack", "discord", "obsidian", "signal", "teams", "brave",
        "edge", "vivaldi",
    ]
    .iter()
    .any(|k| id.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_snake_case() {
        assert_eq!(snake(Role::Button.name()), "button");
        assert_eq!(snake(Role::PasswordText.name()), "password_text");
        assert_eq!(snake(State::ReadOnly.to_static_str()), "read_only");
        assert!(needs_accessibility_switch("code"));
        assert!(!needs_accessibility_switch("org.gnome.TextEditor"));
    }
}

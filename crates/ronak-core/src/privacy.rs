//! Privacy: exclusion rules and detail-level projection.
//!
//! Two independent mechanisms:
//!
//! 1. **Exclusion rules** decide what is *recorded at all*. They run inside
//!    the adapter (so a password manager's title never leaves the compositor)
//!    and again in the daemon, as defense in depth. The GNOME extension
//!    implements the same semantics in `lib/privacy.js`; both are tested
//!    against `tests/fixtures/exclusions.json`.
//! 2. **Detail levels** decide what a *consumer* (an AI agent over MCP, the
//!    LLM tier) may see of what was recorded. See [`project`].

use serde::{Deserialize, Serialize};

use crate::model::{DetailLevel, Snapshot, Window};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionMode {
    /// Keep the window (so layouts still work) but drop its title.
    Redact,
    /// Pretend the window does not exist.
    Ignore,
}

/// Matches when every condition that is set matches. A rule with no
/// conditions never matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExclusionRule {
    /// Case-insensitive glob against the app id; `*` matches any run of characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Case-insensitive substring of the window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    pub mode: ExclusionMode,
}

impl ExclusionRule {
    pub fn app(pattern: &str, mode: ExclusionMode) -> Self {
        Self { app_id: Some(pattern.into()), title_contains: None, mode }
    }
    pub fn title(needle: &str, mode: ExclusionMode) -> Self {
        Self { app_id: None, title_contains: Some(needle.into()), mode }
    }

    pub fn matches(&self, window: &Window) -> bool {
        if self.app_id.is_none() && self.title_contains.is_none() {
            return false;
        }
        if let Some(pattern) = &self.app_id {
            match window.app_id() {
                Some(id) if glob_match(pattern, id) => {}
                _ => return false,
            }
        }
        if let Some(needle) = &self.title_contains {
            match &window.title {
                Some(t) if t.to_lowercase().contains(&needle.to_lowercase()) => {}
                _ => return false,
            }
        }
        true
    }
}

/// Password managers, credential prompts and private browsing, redacted by default.
pub fn default_exclusions() -> Vec<ExclusionRule> {
    use ExclusionMode::Redact;
    vec![
        ExclusionRule::app("*keepassxc*", Redact),
        ExclusionRule::app("*bitwarden*", Redact),
        ExclusionRule::app("*1password*", Redact),
        ExclusionRule::app("*protonpass*", Redact),
        ExclusionRule::app("*proton-pass*", Redact),
        ExclusionRule::app("*gcr-prompter*", Redact),
        ExclusionRule::app("*seahorse*", Redact),
        ExclusionRule::app("*polkit*", Redact),
        ExclusionRule::title("Private Browsing", Redact),
        ExclusionRule::title("Incognito", Redact),
        ExclusionRule::title("InPrivate", Redact),
    ]
}

/// The strongest mode among all matching rules (`Ignore` beats `Redact`).
pub fn evaluate(rules: &[ExclusionRule], window: &Window) -> Option<ExclusionMode> {
    rules.iter().filter(|r| r.matches(window)).map(|r| r.mode).max()
}

/// Applies rules to one window: `None` if it must be ignored, otherwise the
/// window with sensitive fields removed as required.
pub fn apply_exclusions(rules: &[ExclusionRule], mut window: Window, observe_titles: bool) -> Option<Window> {
    match evaluate(rules, &window) {
        Some(ExclusionMode::Ignore) => None,
        Some(ExclusionMode::Redact) => {
            window.title = None;
            window.redacted = true;
            Some(window)
        }
        None => {
            if !observe_titles {
                window.title = None;
            }
            Some(window)
        }
    }
}

/// Reduces a full snapshot to what a consumer at `level` may see.
///
/// | Field | task_only | task_and_apps | full | content |
/// | --- | --- | --- | --- | --- |
/// | task label, activity, confidence | yes | yes | yes | yes |
/// | windows, apps, roles | | yes | yes | yes |
/// | window titles, project, evidence details | | | yes | yes |
/// | elements (structure, names) | | | yes | yes |
/// | element text, notification bodies | | | | yes |
/// | capabilities | | kind and status | + titles | + bodies |
pub fn project(snapshot: &Snapshot, level: DetailLevel) -> Snapshot {
    let mut s = snapshot.clone();
    s.detail = level.min(snapshot.detail);
    let detail = s.detail;

    if let Some(task) = &mut s.task {
        if !detail.allows_titles() {
            // Project names and explanations are derived from titles.
            task.project = None;
            for e in &mut task.evidence {
                if e.signal != "focus_time" || !detail.allows_windows() {
                    e.detail = None;
                }
            }
        }
        if !detail.allows_windows() {
            task.members.clear();
        }
    }

    if !detail.allows_windows() {
        s.windows.clear();
        s.monitors.clear();
        s.workspaces = None;
        s.focused_window = None;
        s.capabilities.clear();
    } else if !detail.allows_titles() {
        for w in &mut s.windows {
            w.title = None;
        }
    }

    if !detail.allows_titles() {
        s.elements.clear();
    } else if !detail.allows_content() {
        for e in &mut s.elements {
            e.text = None;
        }
    }

    for c in &mut s.capabilities {
        if let Some(m) = &mut c.media
            && !detail.allows_titles()
        {
            m.title = None;
            m.artist = None;
        }
        if let Some(cal) = &mut c.calendar {
            for e in cal.current.iter_mut().chain(cal.upcoming.iter_mut()) {
                if !detail.allows_titles() {
                    e.summary = None;
                    e.location = None;
                }
            }
        }
        if let Some(n) = &mut c.notifications {
            for item in &mut n.recent {
                if !detail.allows_titles() {
                    item.title = None;
                }
                if !detail.allows_content() {
                    item.body = None;
                }
            }
        }
    }
    s
}

/// Case-insensitive glob where `*` matches any (possibly empty) run of characters.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Classic two-pointer wildcard match with backtracking to the last '*'.
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn glob() {
        assert!(glob_match("*keepassxc*", "org.keepassxc.KeePassXC"));
        assert!(glob_match("firefox", "Firefox"));
        assert!(glob_match("org.*.nautilus", "org.gnome.Nautilus"));
        assert!(!glob_match("firefox", "firefox-esr"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("a*b", "acd"));
    }

    #[derive(Deserialize)]
    struct Fixture {
        rules: Vec<ExclusionRule>,
        cases: Vec<Case>,
    }
    #[derive(Deserialize)]
    struct Case {
        name: String,
        window: Window,
        expected: String,
    }

    /// The same fixture drives the JavaScript tests, so both sides of the
    /// privacy boundary are guaranteed to agree.
    #[test]
    fn shared_fixture() {
        let f: Fixture = serde_json::from_str(include_str!("../../../tests/fixtures/exclusions.json")).unwrap();
        for case in f.cases {
            let got = match evaluate(&f.rules, &case.window) {
                None => "keep",
                Some(ExclusionMode::Redact) => "redact",
                Some(ExclusionMode::Ignore) => "ignore",
            };
            assert_eq!(got, case.expected, "case '{}'", case.name);
        }
    }

    #[test]
    fn defaults_redact_password_managers() {
        let w = Window {
            id: "1".into(),
            app: Some(crate::model::App {
                id: "org.keepassxc.KeePassXC".into(),
                name: "KeePassXC".into(),
                category: Default::default(),
            }),
            title: Some("Passwords.kdbx - KeePassXC".into()),
            kind: Default::default(),
            rect: Default::default(),
            monitor: 0,
            workspace: Some(0),
            focused: false,
            minimized: false,
            maximized: false,
            fullscreen: false,
            redacted: false,
            last_focused_at: None,
            pid: None,
        };
        let out = apply_exclusions(&default_exclusions(), w, true).unwrap();
        assert!(out.redacted);
        assert!(out.title.is_none());
    }
}

#[cfg(test)]
mod content_tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn content_needs_the_content_level() {
        let now = chrono::Utc::now();
        let mut snap = crate::state::DesktopState::new().snapshot(
            now,
            Some(Task {
                label: "coding".into(),
                project: Some("secret-project".into()),
                evidence: vec![Evidence { signal: "project".into(), detail: Some("from title".into()), weight: 0.3 }],
                ..Task::default()
            }),
        );
        snap.detail = DetailLevel::Content;
        snap.elements.push(Element {
            id: "e".into(),
            window_id: None,
            parent_id: None,
            role: "text".into(),
            name: Some("Message".into()),
            states: vec![],
            text: Some("my private message".into()),
        });
        snap.capabilities.push(Capability {
            id: "notifications".into(),
            kind: CapabilityKind::Notifications,
            provider: "gnome-shell".into(),
            name: "Notifications".into(),
            access: Access::Read,
            actions: vec![],
            media: None,
            calendar: None,
            mcp: None,
            notifications: Some(NotificationsState {
                recent: vec![NotificationInfo {
                    app: "Slack".into(),
                    title: Some("Ana".into()),
                    body: Some("see you".into()),
                    at: now,
                    urgency: Urgency::Normal,
                }],
                count_last_hour: 1,
            }),
        });
        let full = project(&snap, DetailLevel::Full);
        assert!(full.elements[0].text.is_none() && full.elements[0].name.is_some());
        let n = &full.capabilities[0].notifications.as_ref().unwrap().recent[0];
        assert!(n.body.is_none() && n.title.is_some());
        let apps = project(&snap, DetailLevel::TaskAndApps);
        assert!(apps.elements.is_empty());
        assert!(apps.task.as_ref().unwrap().project.is_none());
        assert!(apps.task.as_ref().unwrap().evidence[0].detail.is_none());
        assert_eq!(apps.capabilities[0].notifications.as_ref().unwrap().recent[0].app, "Slack");
        let content = project(&snap, DetailLevel::Content);
        assert_eq!(content.elements[0].text.as_deref(), Some("my private message"));
        // A consumer can never get more than the snapshot holds.
        let mut low = snap.clone();
        low.detail = DetailLevel::TaskOnly;
        assert_eq!(project(&low, DetailLevel::Content).detail, DetailLevel::TaskOnly);
    }
}

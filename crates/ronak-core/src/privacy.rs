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
/// - `TaskOnly`: task label, confidence and source; activity state. Window
///   lists, monitors and task members are emptied.
/// - `TaskAndApps`: windows with app identity, geometry and roles, but no titles.
/// - `Full`: unchanged (exclusion rules were already applied upstream).
pub fn project(snapshot: &Snapshot, level: DetailLevel) -> Snapshot {
    let mut s = snapshot.clone();
    s.detail = level.min(snapshot.detail);
    if !s.detail.allows_windows() {
        s.windows.clear();
        s.monitors.clear();
        s.workspaces = None;
        s.focused_window = None;
        if let Some(task) = &mut s.task {
            task.members.clear();
        }
    } else if !s.detail.allows_titles() {
        for w in &mut s.windows {
            w.title = None;
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
        };
        let out = apply_exclusions(&default_exclusions(), w, true).unwrap();
        assert!(out.redacted);
        assert!(out.title.is_none());
    }
}

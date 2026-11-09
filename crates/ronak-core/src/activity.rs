//! Intent v2: from "what kind of work" to "which part of it, on what, and why".
//!
//! [`refine`] takes the task guessed from focus history and adds:
//! - an **activity** from the taxonomy (coding → debugging), using signals
//!   from inside apps: the focused element, terminal output, document titles;
//! - a **project**, parsed from editor, terminal and browser titles;
//! - **evidence** for each conclusion, and **alternatives**;
//! - corrections from **context capabilities**: a calendar event right now
//!   plus an open meeting app makes "meeting" very likely.
//!
//! Text read from inside apps is used here and nowhere else in the engine; it
//! never appears in evidence details.

use crate::apps;
use crate::model::*;
use crate::state::DesktopState;
use crate::taxonomy;

/// Context gathered by sensors other than the window adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct Signals<'a> {
    /// Elements of the focused app (may include text).
    pub elements: &'a [Element],
    pub capabilities: &'a [Capability],
}

impl Signals<'_> {
    fn focused_element(&self) -> Option<&Element> {
        self.elements.iter().find(|e| e.has_state("focused"))
    }

    /// The last ~2000 characters shown by terminals in the focused app.
    fn terminal_tail(&self) -> String {
        let mut text = String::new();
        for e in self.elements.iter().filter(|e| e.role == "terminal") {
            if let Some(t) = &e.text {
                text.push_str(t);
                text.push('\n');
            }
        }
        let chars: Vec<char> = text.chars().collect();
        chars[chars.len().saturating_sub(2000)..].iter().collect::<String>().to_lowercase()
    }

    fn calendar(&self) -> Option<&CalendarState> {
        self.capabilities.iter().find_map(|c| c.calendar.as_ref())
    }

    fn media_playing(&self) -> bool {
        self.capabilities.iter().filter_map(|c| c.media.as_ref()).any(|m| m.status == PlaybackStatus::Playing)
    }

    fn recent_notifications(&self, now: chrono::DateTime<chrono::Utc>) -> usize {
        self.capabilities
            .iter()
            .filter_map(|c| c.notifications.as_ref())
            .flat_map(|n| n.recent.iter())
            .filter(|n| now - n.at <= chrono::Duration::minutes(5))
            .count()
    }
}

const TEST_MARKERS: &[&str] = &[
    "test result:",
    "running ",
    " passed",
    " failed",
    "pytest",
    "cargo test",
    "npm test",
    "npm run test",
    "go test",
    "jest",
    "vitest",
    "tests passed",
    "assertionerror",
];
const ERROR_MARKERS: &[&str] = &[
    "error[",
    "error:",
    "panicked at",
    "traceback (most recent call last)",
    "exception",
    "segmentation fault",
    "undefined reference",
    "cannot find",
    "stack trace",
];
const REVIEW_MARKERS: &[&str] = &["pull request", "files changed", "merge request", "code review", "review requested"];
const SEARCH_MARKERS: &[&str] = &["google search", "- google", "duckduckgo", "- bing", "search results", "kagi"];

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Adds activity, project, evidence and alternatives to `task`.
pub fn refine(mut task: Task, state: &DesktopState, signals: &Signals, now: chrono::DateTime<chrono::Utc>) -> Task {
    // Context can change the task itself before we look for the activity.
    apply_calendar(&mut task, state, signals);

    let focused = state.focused_window();
    let focused_title = focused.and_then(|w| w.title.as_deref()).unwrap_or("").to_lowercase();
    let focused_cat = focused.map(|w| w.category()).unwrap_or_default();
    let element = signals.focused_element();
    let editing = element.is_some_and(|e| e.has_state("editable") && !e.has_state("read_only"));
    let terminal = signals.terminal_tail();

    let mut scores: Vec<(&'static str, f32, Option<String>)> = Vec::new();
    let mut add =
        |activity: &'static str, weight: f32, why: &str| scores.push((activity, weight, Some(why.to_string())));

    match task.label.as_str() {
        "coding" => {
            if contains_any(&terminal, ERROR_MARKERS) {
                add("debugging", 0.7, "the terminal shows errors");
            }
            if contains_any(&terminal, TEST_MARKERS) {
                add("testing", 0.6, "the terminal shows test output");
            }
            if focused_title.contains("test") || focused_title.contains("spec") {
                add("testing", 0.4, "the open file looks like a test");
            }
            if contains_any(&focused_title, REVIEW_MARKERS) || terminal.contains("git diff") {
                add("reviewing", 0.8, "a code review is open");
            }
            if focused_cat == AppCategory::Browser && apps::looks_like_dev(Some(&focused_title)) && !editing {
                add("reading_docs", 0.6, "you are reading developer documentation");
            }
            if focused_cat == AppCategory::Terminal {
                add("running_commands", 0.35, "a terminal has focus");
            }
            if focused_cat == AppCategory::Editor {
                add("writing_code", if editing { 0.5 } else { 0.3 }, "an editor has focus");
            }
        }
        "writing" => {
            if focused.map(|w| w.app_id().unwrap_or("")).is_some_and(|id| {
                let id = id.to_lowercase();
                id.contains("obsidian") || id.contains("logseq") || id.contains("joplin")
            }) {
                add("note_taking", 0.6, "a notes app has focus");
            }
            if editing {
                add("drafting", 0.5, "you are typing in a document");
            } else {
                add("editing", 0.3, "a document is open but you are not typing");
            }
        }
        "reading" => {
            if focused_cat == AppCategory::Browser {
                add("reading_web", 0.5, "a browser has focus");
            } else {
                add("reading_document", 0.5, "a document viewer has focus");
            }
        }
        "research" => {
            if contains_any(&focused_title, SEARCH_MARKERS) {
                add("searching", 0.6, "a search page is open");
            }
            let browsers =
                state.windows.values().filter(|w| w.category() == AppCategory::Browser && !w.minimized).count();
            if browsers >= 2 {
                add("comparing", 0.4, "several browser windows are open");
            }
            add("reading_web", 0.3, "a browser has focus");
        }
        "communication" => {
            if focused_cat == AppCategory::Mail {
                add("emailing", 0.6, "a mail app has focus");
            } else {
                add("chatting", 0.5, "a chat app has focus");
            }
        }
        "meeting" => {
            if focused_cat == AppCategory::Documents && editing {
                add("taking_notes", 0.6, "you are typing notes during a meeting");
            }
            if focused_title.contains("presenting") || focused_title.contains("sharing") {
                add("presenting", 0.7, "you appear to be sharing your screen");
            }
            add("in_call", 0.4, "a meeting app is open");
        }
        "media" => {
            if focused_cat == AppCategory::Media || focused_title.contains("youtube") {
                add("watching", 0.5, "a video is on screen");
            }
            if signals.media_playing() {
                add("listening", 0.4, "music is playing");
            }
        }
        "design" => add("designing", 0.5, "a design tool has focus"),
        "admin" => {
            if focused_cat == AppCategory::Files {
                add("file_management", 0.6, "the file manager has focus");
            } else if focused_cat == AppCategory::System {
                add("system_settings", 0.6, "system settings are open");
            } else if terminal.contains("sudo ") || terminal.contains("apt ") || terminal.contains("systemctl") {
                add("terminal_admin", 0.6, "the terminal shows system administration");
            }
        }
        _ => {}
    }

    // Keep the strongest reason per activity, then pick the best.
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
    let mut seen: Vec<&str> = vec![];
    scores.retain(|(a, _, _)| {
        let fresh = !seen.contains(a);
        seen.push(a);
        fresh
    });
    if task.source != TaskSource::User || task.activity.is_none() {
        if let Some((activity, weight, why)) = scores.first() {
            task.activity = Some((*activity).to_string());
            task.evidence.push(Evidence { signal: "activity".into(), detail: why.clone(), weight: *weight });
        } else if let Some(default) = taxonomy::activities(&task.label).first() {
            task.activity = Some((*default).to_string());
        }
    }
    for (activity, weight, _) in scores.iter().skip(1).take(2) {
        task.alternatives.push(Alternative {
            label: task.label.clone(),
            activity: Some((*activity).to_string()),
            confidence: (weight * task.confidence).clamp(0.0, 1.0),
        });
    }

    // Context that does not change the task but explains it.
    if signals.media_playing() {
        task.evidence.push(Evidence { signal: "media".into(), detail: Some("music is playing".into()), weight: 0.1 });
    }
    let recent = signals.recent_notifications(now);
    if recent >= 3 && task.label != "communication" {
        task.alternatives.push(Alternative { label: "communication".into(), activity: None, confidence: 0.2 });
        task.evidence.push(Evidence {
            signal: "notifications".into(),
            detail: Some(format!("{recent} notifications in the last 5 minutes")),
            weight: 0.15,
        });
    }

    // A project you named yourself is kept.
    if (task.source != TaskSource::User || task.project.is_none())
        && let Some((project, from)) = detect_project(state, &task)
    {
        task.evidence.push(Evidence {
            signal: "project".into(),
            detail: Some(format!("project \"{project}\" from {from}")),
            weight: 0.3,
        });
        task.project = Some(project);
    }
    task.evidence.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    task
}

/// A calendar event happening now, plus a meeting app or tab on screen,
/// makes "meeting" the task even if the user is glancing elsewhere.
fn apply_calendar(task: &mut Task, state: &DesktopState, signals: &Signals) {
    let Some(cal) = signals.calendar() else { return };
    let Some(event) = cal.current.iter().find(|e| !e.all_day) else { return };
    let meeting_on_screen = state.windows.values().any(|w| w.category() == AppCategory::Meeting && !w.minimized);
    let what = event.summary.as_deref().map(|s| format!("\"{s}\"")).unwrap_or_else(|| "an event".into());
    if meeting_on_screen && task.label != "meeting" && task.source != TaskSource::User {
        task.alternatives
            .insert(0, Alternative { label: task.label.clone(), activity: None, confidence: task.confidence });
        task.label = "meeting".into();
        task.confidence = task.confidence.max(0.8);
        task.activity = None;
        task.evidence.push(Evidence {
            signal: "calendar".into(),
            detail: Some(format!("your calendar has {what} right now and a meeting app is open")),
            weight: 0.8,
        });
    } else if task.label == "meeting" {
        task.confidence = (task.confidence + 0.1).min(1.0);
        task.evidence.push(Evidence {
            signal: "calendar".into(),
            detail: Some(format!("your calendar has {what} right now")),
            weight: 0.5,
        });
    } else {
        task.alternatives.push(Alternative { label: "meeting".into(), activity: None, confidence: 0.25 });
        task.evidence.push(Evidence {
            signal: "calendar".into(),
            detail: Some(format!("your calendar has {what} right now")),
            weight: 0.1,
        });
    }
}

/// Finds the project name in titles of the task's windows (primary first).
/// Returns the name and a description of where it came from.
pub fn detect_project(state: &DesktopState, task: &Task) -> Option<(String, &'static str)> {
    let mut votes: Vec<(String, f32, &'static str)> = vec![];
    for m in &task.members {
        if m.role == Role::Background {
            continue;
        }
        let Some(w) = state.windows.get(&m.window_id) else { continue };
        let Some(title) = w.title.as_deref() else { continue };
        let weight = if m.role == Role::Primary { 2.0 } else { 1.0 };
        let found = match w.category() {
            // VS Code style "file - project - Code", or GNOME Text Editor
            // style "file (~/src/project) - Text Editor".
            AppCategory::Editor => {
                project_from_editor_title(title).or_else(|| project_from_path(title)).map(|p| (p, "the editor title"))
            }
            AppCategory::Terminal => project_from_path(title).map(|p| (p, "the terminal's folder")),
            AppCategory::Browser => project_from_repo_title(title).map(|p| (p, "the repository page")),
            _ => None,
        };
        if let Some((p, from)) = found {
            votes.push((p, weight, from));
        }
    }
    let mut best: Option<(String, f32, &'static str)> = None;
    for (p, _, from) in &votes {
        let total: f32 = votes.iter().filter(|(q, _, _)| q == p).map(|(_, w, _)| w).sum();
        if best.as_ref().is_none_or(|(_, b, _)| total > *b) {
            best = Some((p.clone(), total, from));
        }
    }
    best.map(|(p, _, from)| (p, from))
}

/// "main.rs - ronak - Visual Studio Code" -> "ronak". JetBrains IDEs use
/// "ronak – main.rs".
pub fn project_from_editor_title(title: &str) -> Option<String> {
    let separators = [" - ", " \u{2014} ", " \u{2013} "];
    for sep in separators {
        let pieces: Vec<&str> = title.split(sep).map(str::trim).filter(|p| !p.is_empty()).collect();
        if pieces.len() >= 3 {
            let last = pieces[pieces.len() - 1].to_lowercase();
            if ["visual studio code", "code - oss", "vscodium", "cursor", "zed"].iter().any(|e| last.contains(e)) {
                return clean_project(pieces[pieces.len() - 2]);
            }
        }
        if sep == " \u{2013} " && pieces.len() == 2 {
            return clean_project(pieces[0]);
        }
    }
    None
}

/// Takes the last folder of a path in a title: "me@host: ~/Projects/ronak" -> "ronak".
pub fn project_from_path(title: &str) -> Option<String> {
    let start = title.find("~/").or_else(|| title.find("/home/"))?;
    let path: String = title[start..].chars().take_while(|c| !c.is_whitespace() && *c != ')' && *c != ']').collect();
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    if last == "~" || last.is_empty() {
        return None;
    }
    clean_project(last)
}

/// "abdullah-celik/Ronak: a desktop layer" or "Pull Request #4 · owner/repo" -> "Ronak"/"repo".
pub fn project_from_repo_title(title: &str) -> Option<String> {
    let lower = title.to_lowercase();
    if !(lower.contains("github") || lower.contains("gitlab") || lower.contains("codeberg") || lower.contains(" · ")) {
        return None;
    }
    title
        .split(|c: char| c.is_whitespace() || c == ':' || c == '\u{b7}')
        .filter_map(|tok| {
            let (owner, repo) = tok.split_once('/')?;
            let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c));
            (ok(owner) && ok(repo) && !repo.contains('/')).then(|| repo.to_string())
        })
        .next()
        .and_then(|r| clean_project(&r))
}

fn clean_project(raw: &str) -> Option<String> {
    let p = raw.trim().trim_matches(|c: char| c == '[' || c == ']' || c == '(' || c == ')');
    let ok = !p.is_empty() && p.chars().count() <= 60 && !p.contains("://");
    ok.then(|| p.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::{HeuristicIntent, IntentModel};
    use crate::protocol::PlatformEvent;
    use crate::state::tests::win;
    use chrono::{DateTime, Duration, Utc};

    fn t(s: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T10:00:00Z").unwrap().with_timezone(&Utc) + Duration::seconds(s)
    }

    #[test]
    fn project_parsing() {
        assert_eq!(project_from_editor_title("main.rs - ronak - Visual Studio Code").as_deref(), Some("ronak"));
        assert_eq!(project_from_editor_title("● daemon.rs \u{2014} ronak \u{2014} Cursor").as_deref(), Some("ronak"));
        assert_eq!(project_from_editor_title("ronak \u{2013} main.rs").as_deref(), Some("ronak"));
        assert_eq!(project_from_editor_title("Untitled"), None);
        assert_eq!(project_from_path("abdullah@laptop: ~/Projects/ronak").as_deref(), Some("ronak"));
        assert_eq!(project_from_path("nvim (~/code/site/)").as_deref(), Some("site"));
        assert_eq!(project_from_path("~"), None);
        // GNOME Text Editor and gedit put the folder in the title.
        assert_eq!(project_from_path("main.rs (~/src/ronak) - Text Editor").as_deref(), Some("ronak"));
        assert_eq!(
            project_from_repo_title("abdullah-celik/Ronak: Local-first context - GitHub").as_deref(),
            Some("Ronak")
        );
        assert_eq!(
            project_from_repo_title("Add panel by x · Pull Request #4 · abdullah-celik/Ronak").as_deref(),
            Some("Ronak")
        );
        assert_eq!(project_from_repo_title("Recipes and/or desserts"), None);
    }

    fn coding_desk(terminal_title: &str) -> DesktopState {
        let mut s = DesktopState::new();
        let area = Rect::new(0, 32, 1920, 1168);
        s.apply(
            &PlatformEvent::Snapshot {
                monitors: vec![Monitor {
                    index: 0,
                    geometry: Rect::new(0, 0, 1920, 1200),
                    work_area: area,
                    scale: 1.0,
                    primary: true,
                }],
                workspaces: None,
                windows: vec![
                    win("1", "code", "daemon.rs - ronak - Visual Studio Code", area),
                    win("2", "Alacritty", terminal_title, area),
                    win("3", "us.zoom.Zoom", "Zoom Meeting", area),
                ],
                focused_window: Some("1".into()),
                idle_ms: 0,
                session: Default::default(),
            },
            t(0),
        );
        s.apply(&PlatformEvent::FocusChanged { window_id: Some("2".into()) }, t(200));
        s
    }

    fn terminal(text: &str) -> Element {
        Element {
            id: "e1".into(),
            window_id: Some("2".into()),
            parent_id: None,
            role: "terminal".into(),
            name: None,
            states: vec!["focused".into()],
            text: Some(text.into()),
        }
    }

    #[test]
    fn terminal_output_reveals_debugging_and_project() {
        let s = coding_desk("me@laptop: ~/Projects/ronak");
        let task = HeuristicIntent::default().infer(&s, t(260)).unwrap();
        assert_eq!(task.label, "coding");
        let elements = [terminal("$ cargo build\nerror[E0308]: mismatched types\n")];
        let refined = refine(task, &s, &Signals { elements: &elements, capabilities: &[] }, t(260));
        assert_eq!(refined.activity.as_deref(), Some("debugging"));
        assert_eq!(refined.project.as_deref(), Some("ronak"));
        assert!(refined.evidence.iter().any(|e| e.signal == "activity"));
        // Evidence never quotes what was read inside the terminal.
        assert!(refined.evidence.iter().all(|e| !e.detail.as_deref().unwrap_or("").contains("mismatched")));
    }

    #[test]
    fn calendar_plus_meeting_app_means_meeting() {
        let s = coding_desk("zsh");
        let task = HeuristicIntent::default().infer(&s, t(260)).unwrap();
        let cal = Capability {
            id: "calendar".into(),
            kind: CapabilityKind::Calendar,
            provider: "evolution-data-server".into(),
            name: "Calendar".into(),
            access: Access::Read,
            actions: vec![],
            media: None,
            calendar: Some(CalendarState {
                current: vec![CalendarEvent {
                    summary: Some("Design review".into()),
                    start: t(0),
                    end: t(3600),
                    location: None,
                    all_day: false,
                }],
                upcoming: vec![],
            }),
            notifications: None,
            mcp: None,
        };
        let refined = refine(task, &s, &Signals { elements: &[], capabilities: &[cal] }, t(260));
        assert_eq!(refined.label, "meeting");
        assert_eq!(refined.activity.as_deref(), Some("in_call"));
        assert_eq!(refined.alternatives[0].label, "coding");
    }
}

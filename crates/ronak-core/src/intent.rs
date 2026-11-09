//! The intent engine: what is the user doing, and what role does each window
//! play in it?
//!
//! Everything that guesses intent implements [`IntentModel`]. Today that is a
//! transparent heuristic over focus history. Later a small classifier (tier 1,
//! ideally on an NPU) and a local LLM (tier 2) implement the same trait, and
//! the daemon can escalate from one to the next when confidence is low.
//!
//! [`TaskTracker`] adds hysteresis on top, so a quick glance at a chat window
//! does not flip the whole desktop from "coding" to "communication".

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};

use crate::model::{
    Alternative, AppCategory, Evidence, KNOWN_TASKS, Role, Task, TaskMember, TaskSource, Window, WindowKind,
};
use crate::state::DesktopState;

pub trait IntentModel: Send + Sync {
    fn name(&self) -> &'static str;
    /// Returns the best guess, or `None` when there is nothing to reason about.
    fn infer(&self, state: &DesktopState, now: DateTime<Utc>) -> Option<Task>;
}

/// Transparent, rule-based intent model. Every number here is a knob worth
/// experimenting with; the point is to have a baseline the learned models
/// must beat.
#[derive(Debug, Clone)]
pub struct HeuristicIntent {
    /// How far back focus history counts.
    pub lookback: Duration,
    /// Focus time this many seconds ago counts half as much as focus now.
    pub half_life_secs: f64,
    /// Below this much (decayed) evidence, confidence is scaled down.
    pub min_evidence_secs: f64,
    /// At most this many reference windows per task.
    pub max_references: usize,
}

impl Default for HeuristicIntent {
    fn default() -> Self {
        Self { lookback: Duration::minutes(10), half_life_secs: 180.0, min_evidence_secs: 20.0, max_references: 2 }
    }
}

/// How much focus time in a category counts towards each task.
fn task_contributions(cat: AppCategory, dev_hint: bool) -> &'static [(&'static str, f64)] {
    use AppCategory::*;
    match (cat, dev_hint) {
        (Editor, _) => &[("coding", 1.0)],
        (Terminal, _) => &[("coding", 0.9), ("admin", 0.3)],
        (Browser, true) => &[("coding", 0.45), ("research", 0.5)],
        (Browser, false) => &[("research", 1.0)],
        (Reader, _) => &[("reading", 1.0), ("research", 0.3)],
        (Documents, _) => &[("writing", 1.0)],
        (Communication, _) => &[("communication", 1.0)],
        (Mail, _) => &[("communication", 0.9)],
        (Meeting, _) => &[("meeting", 3.0)],
        (Design, _) => &[("design", 1.0)],
        (Media, _) => &[("media", 0.6)],
        (Files, _) => &[("admin", 0.6)],
        (System, _) => &[("admin", 0.8)],
        (Other, _) => &[],
    }
}

/// How relevant a window of category `cat` is to task `label`, 0.0 to 1.0.
pub fn affinity(label: &str, cat: AppCategory, dev_hint: bool) -> f32 {
    use AppCategory::*;
    match label {
        "coding" => match cat {
            Editor => 1.0,
            Terminal => 0.9,
            Browser if dev_hint => 0.8,
            Browser => 0.55,
            Reader => 0.5,
            Documents => 0.4,
            Communication | Files => 0.3,
            Mail => 0.25,
            Media => 0.15,
            _ => 0.1,
        },
        "writing" => match cat {
            Documents => 1.0,
            Editor => 0.7,
            Browser | Reader => 0.6,
            Communication | Mail => 0.25,
            Terminal => 0.2,
            Media => 0.15,
            _ => 0.1,
        },
        "reading" => match cat {
            Reader => 1.0,
            Documents | Browser => 0.6,
            Editor => 0.4,
            Communication => 0.25,
            Media => 0.15,
            _ => 0.1,
        },
        "research" => match cat {
            Browser => 1.0,
            Reader => 0.8,
            Documents => 0.7,
            Editor => 0.5,
            Terminal => 0.3,
            Communication => 0.25,
            Media => 0.15,
            _ => 0.1,
        },
        "communication" => match cat {
            Communication | Mail => 1.0,
            Meeting => 0.6,
            Browser => 0.5,
            Documents => 0.4,
            Media => 0.15,
            _ => 0.2,
        },
        "meeting" => match cat {
            Meeting => 1.0,
            Documents => 0.6,
            Browser | Communication => 0.5,
            Reader => 0.4,
            Editor => 0.3,
            _ => 0.1,
        },
        "design" => match cat {
            Design => 1.0,
            Browser => 0.6,
            Reader => 0.5,
            Files => 0.4,
            Communication => 0.25,
            Media => 0.15,
            _ => 0.1,
        },
        "media" => match cat {
            Media => 1.0,
            Browser => 0.5,
            _ => 0.1,
        },
        "admin" => match cat {
            System | Files => 1.0,
            Terminal => 0.8,
            Browser => 0.4,
            _ => 0.1,
        },
        _ => 0.5,
    }
}

/// Categories that are usually glanced at rather than worked in.
fn is_monitor_category(cat: AppCategory) -> bool {
    matches!(cat, AppCategory::Communication | AppCategory::Mail | AppCategory::Media)
}

/// Deterministic tie-breaking: known tasks in a fixed order, unknown ones last.
fn task_rank(label: &str) -> usize {
    KNOWN_TASKS.iter().position(|t| *t == label).unwrap_or(usize::MAX)
}

/// How much focus time in windows that have since closed still counts.
const CLOSED_WINDOW_WEIGHT: f64 = 0.25;

/// Decayed focus evidence gathered from the focus history.
struct FocusEvidence<'a> {
    window_secs: HashMap<&'a str, f64>,
    scores: BTreeMap<&'static str, f64>,
    total_secs: f64,
}

impl HeuristicIntent {
    fn evidence<'a>(&self, state: &'a DesktopState, now: DateTime<Utc>) -> FocusEvidence<'a> {
        let mut ev = FocusEvidence { window_secs: HashMap::new(), scores: BTreeMap::new(), total_secs: 0.0 };
        for span in state.focus_spans(self.lookback, now) {
            let mid = span.start + (span.end - span.start) / 2;
            let age = (now - mid).num_milliseconds().max(0) as f64 / 1000.0;
            let mut secs = span.seconds() * 0.5f64.powf(age / self.half_life_secs);
            match state.windows.get_key_value(&span.window_id) {
                Some((id, _)) => *ev.window_secs.entry(id.as_str()).or_default() += secs,
                // A closed window is strong evidence its activity is over
                // (the meeting ended, the document was closed).
                None => secs *= CLOSED_WINDOW_WEIGHT,
            }
            ev.total_secs += secs;
            for (label, weight) in task_contributions(span.category, span.dev_hint) {
                *ev.scores.entry(label).or_default() += secs * weight;
            }
        }
        ev
    }

    /// Window roles for a task label decided elsewhere (for example by the
    /// LLM tier), using the same focus evidence as [`IntentModel::infer`].
    pub fn members_for(&self, state: &DesktopState, now: DateTime<Utc>, label: &str) -> Vec<TaskMember> {
        let candidates = layout_candidates(state);
        if candidates.is_empty() {
            return vec![];
        }
        let ev = self.evidence(state, now);
        assign_roles(label, &candidates, &ev.window_secs, state.focused.as_deref(), self.max_references)
    }
}

impl IntentModel for HeuristicIntent {
    fn name(&self) -> &'static str {
        "heuristic"
    }

    fn infer(&self, state: &DesktopState, now: DateTime<Utc>) -> Option<Task> {
        let candidates = layout_candidates(state);
        if candidates.is_empty() {
            return None;
        }

        // 1. Decayed focus seconds per window and per task.
        let FocusEvidence { window_secs, scores, total_secs: evidence } = self.evidence(state, now);

        // 2. Pick the task. With no history yet, fall back to the focused window.
        let total: f64 = scores.values().sum();
        let (label, confidence) = if total > 0.0 {
            let (label, best) = scores
                .iter()
                .max_by(|a, b| a.1.total_cmp(b.1).then(task_rank(b.0).cmp(&task_rank(a.0))))
                .map(|(l, s)| (*l, *s))?;
            let sufficiency = (evidence / self.min_evidence_secs).min(1.0);
            (label.to_string(), ((best / total) * sufficiency) as f32)
        } else {
            let focused = state.focused_window()?;
            let label = task_contributions(focused.category(), dev_hint(focused)).first()?.0;
            (label.to_string(), 0.2)
        };

        // 3. Assign roles to the windows on screen.
        let members = assign_roles(&label, &candidates, &window_secs, state.focused.as_deref(), self.max_references);

        // 4. Explain: the windows that contributed most, and the runners-up.
        let evidence = focus_evidence(state, &window_secs, evidence);
        let alternatives = if total > 0.0 {
            let mut others: Vec<(&str, f64)> =
                scores.iter().filter(|(l, _)| **l != label).map(|(l, s)| (*l, *s)).collect();
            others.sort_by(|a, b| b.1.total_cmp(&a.1).then(task_rank(a.0).cmp(&task_rank(b.0))));
            others
                .into_iter()
                .take(2)
                .filter(|(_, s)| *s / total >= 0.05)
                .map(|(l, s)| Alternative { label: l.to_string(), activity: None, confidence: (s / total) as f32 })
                .collect()
        } else {
            vec![]
        };
        Some(Task {
            label,
            confidence,
            source: TaskSource::Heuristic,
            since: now,
            members,
            evidence,
            alternatives,
            ..Task::default()
        })
    }
}

/// "Code had focus for 62% of the last 10 minutes", for the top windows.
/// Mentions app names only, never titles.
fn focus_evidence(state: &DesktopState, window_secs: &HashMap<&str, f64>, total_secs: f64) -> Vec<Evidence> {
    if total_secs <= 0.0 {
        return vec![];
    }
    let mut per_app: BTreeMap<String, f64> = BTreeMap::new();
    for (id, secs) in window_secs {
        if let Some(w) = state.windows.get(*id) {
            *per_app.entry(w.app_name().to_string()).or_default() += secs;
        }
    }
    let mut apps: Vec<(String, f64)> = per_app.into_iter().collect();
    apps.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    apps.into_iter()
        .take(3)
        .map(|(app, secs)| {
            let share = secs / total_secs;
            Evidence {
                signal: "focus_time".into(),
                detail: Some(format!("{app} had {:.0}% of your recent attention", share * 100.0)),
                weight: share as f32,
            }
        })
        .filter(|e| e.weight >= 0.05)
        .collect()
}

fn dev_hint(w: &Window) -> bool {
    crate::apps::looks_like_dev(w.title.as_deref())
}

/// Windows a layout may arrange: normal, visible, on the active workspace and
/// on the monitor the user is working on.
pub fn layout_candidates(state: &DesktopState) -> Vec<&Window> {
    let monitor = state.active_monitor().map(|m| m.index);
    state
        .windows
        .values()
        .filter(|w| w.kind == WindowKind::Normal && !w.minimized && !w.fullscreen)
        .filter(|w| state.on_active_workspace(w))
        .filter(|w| monitor.is_none_or(|m| w.monitor == m))
        .collect()
}

fn assign_roles(
    label: &str,
    candidates: &[&Window],
    window_secs: &HashMap<&str, f64>,
    focused: Option<&str>,
    max_references: usize,
) -> Vec<TaskMember> {
    struct Scored<'a> {
        w: &'a Window,
        affinity: f32,
        relevance: f64,
        used: bool,
    }
    let mut scored: Vec<Scored> = candidates
        .iter()
        .map(|w| {
            let affinity = affinity(label, w.category(), dev_hint(w));
            let secs = window_secs.get(w.id.as_str()).copied().unwrap_or(0.0);
            let mut relevance = affinity as f64 * (secs + 5.0);
            if focused == Some(w.id.as_str()) {
                relevance += affinity as f64 * 60.0;
            }
            Scored { w, affinity, relevance, used: secs > 0.5 }
        })
        .collect();
    scored.sort_by(|a, b| b.relevance.total_cmp(&a.relevance).then(a.w.id.cmp(&b.w.id)));
    let max_relevance = scored.first().map(|s| s.relevance).unwrap_or(1.0).max(1e-9);

    // Primary: the focused window if it fits the task, else the best fit.
    let primary_idx = scored
        .iter()
        .position(|s| focused == Some(s.w.id.as_str()) && s.affinity >= 0.5)
        .or_else(|| scored.iter().position(|s| s.affinity >= 0.8))
        .unwrap_or(0);
    let task_is_social = matches!(label, "communication" | "media" | "meeting");

    let mut roles: Vec<Role> = vec![Role::Background; scored.len()];
    roles[primary_idx] = Role::Primary;
    let mut monitors = 0;
    let mut references = 0;
    for (i, s) in scored.iter().enumerate() {
        if i == primary_idx {
            continue;
        }
        if !task_is_social && is_monitor_category(s.w.category()) {
            if monitors == 0 && s.used {
                roles[i] = Role::Monitor;
                monitors += 1;
            }
        } else if s.affinity >= 0.5 && references < max_references {
            roles[i] = Role::Reference;
            references += 1;
        }
    }

    scored
        .iter()
        .zip(roles)
        .map(|(s, role)| TaskMember {
            window_id: s.w.id.clone(),
            role,
            weight: ((s.relevance / max_relevance) as f32).clamp(0.05, 1.0),
        })
        .collect()
}

/// What changed when the tracker considered a new candidate.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackerUpdate {
    Unchanged,
    Started(Task),
    Changed {
        from: Task,
        to: Task,
    },
    /// Same task; members, roles or confidence were refreshed.
    Refined(Task),
}

/// Adds hysteresis to raw guesses so the task does not flicker.
#[derive(Debug, Clone)]
pub struct TaskTracker {
    current: Option<Task>,
    pending: Option<(String, u32)>,
    /// A user correction holds until this time.
    pinned_until: Option<DateTime<Utc>>,
    /// A different label must win this many consecutive times to take over...
    pub switch_after: u32,
    /// ...unless it beats the current confidence by this margin.
    pub switch_margin: f32,
    pub min_confidence: f32,
}

impl TaskTracker {
    pub fn new(min_confidence: f32) -> Self {
        Self { current: None, pending: None, pinned_until: None, switch_after: 2, switch_margin: 0.2, min_confidence }
    }

    pub fn current(&self) -> Option<&Task> {
        self.current.as_ref()
    }

    /// Replaces the current task outright, e.g. when the user names it.
    pub fn force(&mut self, task: Task) {
        self.pending = None;
        self.current = Some(task);
    }

    /// Applies a user correction and keeps it, against contrary guesses,
    /// until `until`. Guesses with the same label still refresh the roles.
    pub fn correct(&mut self, task: Task, until: DateTime<Utc>) {
        self.force(task);
        self.pinned_until = Some(until);
    }

    pub fn is_pinned(&self, now: DateTime<Utc>) -> bool {
        self.pinned_until.is_some_and(|t| now < t)
    }

    pub fn update(&mut self, candidate: Option<Task>, now: DateTime<Utc>) -> TrackerUpdate {
        let Some(mut cand) = candidate else {
            return TrackerUpdate::Unchanged;
        };
        let Some(current) = self.current.clone() else {
            if cand.confidence < self.min_confidence {
                return TrackerUpdate::Unchanged;
            }
            cand.since = now;
            self.current = Some(cand.clone());
            return TrackerUpdate::Started(cand);
        };
        if self.is_pinned(now) {
            if cand.label != current.label {
                return TrackerUpdate::Unchanged;
            }
            // Keep what the user said; take fresh roles from the engine.
            let mut kept = current.clone();
            kept.members = cand.members;
            self.current = Some(kept.clone());
            return TrackerUpdate::Refined(kept);
        }
        if cand.label == current.label {
            self.pending = None;
            cand.since = current.since;
            self.current = Some(cand.clone());
            return TrackerUpdate::Refined(cand);
        }
        if cand.confidence < self.min_confidence {
            return TrackerUpdate::Unchanged;
        }
        let streak = match &self.pending {
            Some((label, n)) if *label == cand.label => n + 1,
            _ => 1,
        };
        if streak >= self.switch_after || cand.confidence >= current.confidence + self.switch_margin {
            self.pending = None;
            cand.since = now;
            self.current = Some(cand.clone());
            TrackerUpdate::Changed { from: current, to: cand }
        } else {
            self.pending = Some((cand.label.clone(), streak));
            TrackerUpdate::Unchanged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Monitor, Rect, Workspaces};
    use crate::protocol::PlatformEvent;
    use crate::state::tests::win;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T10:00:00Z").unwrap().with_timezone(&Utc) + Duration::seconds(secs)
    }

    fn desk() -> DesktopState {
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
                workspaces: Some(Workspaces { active: 0, count: 1 }),
                windows: vec![
                    win("10", "code", "main.rs - ronak", area),
                    win("11", "Alacritty", "cargo test", area),
                    win("12", "firefox", "tokio - docs.rs", area),
                    win("13", "com.slack.Slack", "Slack", area),
                    win("14", "org.gnome.Nautilus", "Home", area),
                ],
                focused_window: None,
                idle_ms: 0,
                session: Default::default(),
            },
            t(0),
        );
        s
    }

    fn focus(s: &mut DesktopState, id: &str, at: i64) {
        s.apply(&PlatformEvent::FocusChanged { window_id: Some(id.into()) }, t(at));
    }

    #[test]
    fn coding_session_is_recognized_with_roles() {
        let mut s = desk();
        focus(&mut s, "10", 0);
        focus(&mut s, "11", 120);
        focus(&mut s, "12", 150);
        focus(&mut s, "13", 180);
        focus(&mut s, "10", 190);
        let task = HeuristicIntent::default().infer(&s, t(300)).unwrap();
        assert_eq!(task.label, "coding");
        assert!(task.confidence > 0.6, "confidence {}", task.confidence);
        let role = |id: &str| task.member(id).unwrap().role;
        assert_eq!(role("10"), Role::Primary);
        assert_eq!(role("11"), Role::Reference);
        assert_eq!(role("12"), Role::Reference);
        assert_eq!(role("13"), Role::Monitor);
        assert_eq!(role("14"), Role::Background);
    }

    #[test]
    fn tracker_resists_glances() {
        let mut tracker = TaskTracker::new(0.3);
        let task = |label: &str, c: f32| Task {
            label: label.into(),
            confidence: c,
            source: TaskSource::Heuristic,
            since: t(0),
            ..Task::default()
        };
        assert!(matches!(tracker.update(Some(task("coding", 0.7)), t(0)), TrackerUpdate::Started(_)));
        assert_eq!(tracker.update(Some(task("communication", 0.6)), t(5)), TrackerUpdate::Unchanged);
        assert!(matches!(tracker.update(Some(task("coding", 0.7)), t(10)), TrackerUpdate::Refined(_)));
        assert_eq!(tracker.update(Some(task("communication", 0.6)), t(15)), TrackerUpdate::Unchanged);
        assert!(matches!(tracker.update(Some(task("communication", 0.6)), t(20)), TrackerUpdate::Changed { .. }));
        assert_eq!(tracker.current().unwrap().since, t(20));
    }

    #[test]
    fn corrections_hold_until_they_expire() {
        let mut tracker = TaskTracker::new(0.3);
        let task = |label: &str, c: f32| Task { label: label.into(), confidence: c, since: t(0), ..Task::default() };
        let mut user = task("writing", 1.0);
        user.source = TaskSource::User;
        user.activity = Some("drafting".into());
        tracker.correct(user, t(300));
        for s in [10, 20, 30] {
            assert_eq!(tracker.update(Some(task("coding", 0.99)), t(s)), TrackerUpdate::Unchanged);
        }
        let TrackerUpdate::Refined(kept) = tracker.update(Some(task("writing", 0.5)), t(40)) else { panic!() };
        assert_eq!((kept.source, kept.activity.as_deref()), (TaskSource::User, Some("drafting")));
        // After the pin expires, normal hysteresis applies again.
        assert_eq!(tracker.update(Some(task("coding", 0.99)), t(400)), TrackerUpdate::Unchanged);
        assert!(matches!(tracker.update(Some(task("coding", 0.99)), t(410)), TrackerUpdate::Changed { .. }));
    }
}

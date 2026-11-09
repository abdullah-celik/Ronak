//! Builds a suggestion from the current task: a layout plus any policy
//! actions (such as Do Not Disturb during meetings), with a one-line summary
//! the user sees before accepting.

use chrono::{DateTime, Duration, Utc};

use crate::intent::layout_candidates;
use crate::layout::{self, LayoutConfig, LayoutItem, LayoutPlan};
use crate::model::{Role, Task};
use crate::protocol::{Action, SuggestionView};
use crate::state::DesktopState;

/// A window that moved this recently is assumed to be under the user's hand.
const DRAG_GRACE: Duration = Duration::seconds(3);

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub task_label: String,
    pub summary: String,
    pub actions: Vec<Action>,
    pub plan: LayoutPlan,
}

impl Suggestion {
    pub fn view(&self) -> SuggestionView {
        SuggestionView { id: self.id.clone(), task_label: self.task_label.clone(), summary: self.summary.clone() }
    }

    /// True if applying `other` would do the same thing as applying `self`.
    pub fn same_effect(&self, other: &Suggestion) -> bool {
        self.actions == other.actions
    }
}

/// Inputs from user policy that shape suggestions.
#[derive(Debug, Clone, Default)]
pub struct Policy<'a> {
    /// Tasks during which notifications should be silenced.
    pub dnd_tasks: &'a [String],
    /// Ronak turned Do Not Disturb on earlier and may turn it off again.
    pub dnd_set_by_ronak: bool,
    /// Pixels on the right of the work area kept free for the side panel.
    pub reserve_right: i32,
}

pub fn build(
    id: String,
    state: &DesktopState,
    task: &Task,
    layout_cfg: &LayoutConfig,
    policy: &Policy,
    now: DateTime<Utc>,
) -> Option<Suggestion> {
    let mut area = state.active_monitor()?.work_area;
    area.w = (area.w - policy.reserve_right.max(0)).max(0);
    let items: Vec<LayoutItem> = layout_candidates(state)
        .into_iter()
        .filter_map(|w| {
            let m = task.member(&w.id)?;
            Some(LayoutItem {
                window_id: w.id.clone(),
                role: m.role,
                // Quarter steps: small shifts in attention must not reshuffle the screen.
                weight: ((m.weight * 4.0).round() / 4.0).max(0.25),
                current: w.rect,
                maximized: w.maximized,
                pinned: state.recently_moved(&w.id, DRAG_GRACE, now),
            })
        })
        .collect();
    let plan = layout::solve(area, &items, layout_cfg);

    let mut actions = Vec::new();
    if !plan.is_empty() {
        actions.push(Action::Place { placements: plan.placements.clone() });
    }
    let wants_dnd = policy.dnd_tasks.contains(&task.label);
    let mut dnd_note = None;
    if wants_dnd && !state.session.do_not_disturb {
        actions.push(Action::SetDoNotDisturb { enabled: true });
        dnd_note = Some("notifications paused");
    } else if !wants_dnd && state.session.do_not_disturb && policy.dnd_set_by_ronak {
        actions.push(Action::SetDoNotDisturb { enabled: false });
        dnd_note = Some("notifications back on");
    }
    if actions.is_empty() {
        return None;
    }

    let summary = summarize(state, task, &plan, dnd_note);
    Some(Suggestion { id, created_at: now, task_label: task.label.clone(), summary, actions, plan })
}

/// e.g. "coding: Code main · Alacritty, Firefox side · notifications paused".
fn summarize(state: &DesktopState, task: &Task, plan: &LayoutPlan, dnd_note: Option<&str>) -> String {
    let names = |role: Role| -> Vec<String> {
        plan.placements
            .iter()
            .filter(|p| task.member(&p.window_id).is_some_and(|m| m.role == role))
            .filter_map(|p| state.windows.get(&p.window_id))
            .map(|w| w.app_name().to_string())
            .collect()
    };
    let mut parts = Vec::new();
    let primary = names(Role::Primary);
    if !primary.is_empty() {
        parts.push(format!("{} main", primary.join(", ")));
    }
    let side: Vec<String> = names(Role::Reference).into_iter().chain(names(Role::Monitor)).collect();
    if !side.is_empty() {
        parts.push(format!("{} side", side.join(", ")));
    }
    if let Some(note) = dnd_note {
        parts.push(note.to_string());
    }
    format!("{}: {}", task.label, parts.join(" · "))
}

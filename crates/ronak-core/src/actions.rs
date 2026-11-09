//! Reversible actions and the undo stack.
//!
//! Before Ronak changes anything, it computes how to change it back from the
//! current state. One keypress can therefore always restore what you had.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use crate::protocol::{Action, Placement};
use crate::state::DesktopState;

/// The actions that undo `action`, computed from the state *before* it runs.
pub fn inverse(action: &Action, state: &DesktopState) -> Vec<Action> {
    match action {
        Action::Place { placements } => {
            let back: Vec<Placement> = placements
                .iter()
                .filter_map(|p| state.windows.get(&p.window_id))
                .map(|w| Placement { window_id: w.id.clone(), rect: w.rect, maximized: w.maximized })
                .collect();
            if back.is_empty() { vec![] } else { vec![Action::Place { placements: back }] }
        }
        Action::Focus { .. } => match &state.focused {
            Some(prev) => vec![Action::Focus { window_id: prev.clone() }],
            None => vec![],
        },
        Action::Minimize { window_id } => vec![Action::Unminimize { window_id: window_id.clone() }],
        Action::Unminimize { window_id } => vec![Action::Minimize { window_id: window_id.clone() }],
        Action::MoveToWorkspace { window_id, .. } => match state.windows.get(window_id).and_then(|w| w.workspace) {
            Some(ws) => vec![Action::MoveToWorkspace { window_id: window_id.clone(), workspace: ws }],
            None => vec![],
        },
        Action::ActivateWorkspace { .. } => match state.workspaces {
            Some(ws) => vec![Action::ActivateWorkspace { workspace: ws.active }],
            None => vec![],
        },
        Action::CreateWorkspace => vec![],
        Action::SetDoNotDisturb { .. } => vec![Action::SetDoNotDisturb { enabled: state.session.do_not_disturb }],
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UndoEntry {
    pub label: String,
    pub at: DateTime<Utc>,
    /// Run these, in order, to undo.
    pub inverse: Vec<Action>,
}

#[derive(Debug, Clone)]
pub struct UndoStack {
    entries: VecDeque<UndoEntry>,
    cap: usize,
}

impl UndoStack {
    pub fn new(cap: usize) -> Self {
        Self { entries: VecDeque::new(), cap: cap.max(1) }
    }

    /// Records how to undo `actions`. Call *before* executing them.
    pub fn record(&mut self, label: impl Into<String>, actions: &[Action], state: &DesktopState, now: DateTime<Utc>) {
        // Undo runs in reverse order of the original actions.
        let inverse: Vec<Action> = actions.iter().rev().flat_map(|a| inverse(a, state)).collect();
        if inverse.is_empty() {
            return;
        }
        if self.entries.len() == self.cap {
            self.entries.pop_front();
        }
        self.entries.push_back(UndoEntry { label: label.into(), at: now, inverse });
    }

    pub fn pop(&mut self) -> Option<UndoEntry> {
        self.entries.pop_back()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rect;
    use crate::protocol::PlatformEvent;
    use crate::state::tests::win;

    #[test]
    fn place_and_dnd_are_undone_in_reverse() {
        let now = Utc::now();
        let mut s = DesktopState::new();
        let mut w = win("1", "code", "x", Rect::new(10, 10, 500, 400));
        w.maximized = true;
        s.apply(&PlatformEvent::WindowOpened { window: w }, now);
        let actions = vec![
            Action::Place {
                placements: vec![Placement {
                    window_id: "1".into(),
                    rect: Rect::new(0, 0, 900, 900),
                    maximized: false,
                }],
            },
            Action::SetDoNotDisturb { enabled: true },
        ];
        let mut undo = UndoStack::new(5);
        undo.record("test", &actions, &s, now);
        let entry = undo.pop().unwrap();
        assert_eq!(entry.inverse[0], Action::SetDoNotDisturb { enabled: false });
        assert_eq!(
            entry.inverse[1],
            Action::Place {
                placements: vec![Placement {
                    window_id: "1".into(),
                    rect: Rect::new(10, 10, 500, 400),
                    maximized: true
                }]
            }
        );
        assert!(undo.is_empty());
    }
}

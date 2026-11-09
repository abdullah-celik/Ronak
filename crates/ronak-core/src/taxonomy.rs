//! The fixed vocabulary of tasks and the activities within them.
//!
//! A task says *what kind* of work you are doing (coding); an activity says
//! *which part* of it (debugging). The vocabulary is fixed so that labels are
//! comparable over time and across people; users can still add their own
//! labels when correcting Ronak, and those are kept as they are.

/// Every known task with its activities. The first activity is the default.
pub const TAXONOMY: &[(&str, &[&str])] = &[
    ("coding", &["writing_code", "debugging", "testing", "reviewing", "reading_docs", "running_commands"]),
    ("writing", &["drafting", "editing", "note_taking"]),
    ("reading", &["reading_document", "reading_web"]),
    ("research", &["reading_web", "searching", "comparing"]),
    ("communication", &["chatting", "emailing"]),
    ("meeting", &["in_call", "taking_notes", "presenting"]),
    ("design", &["designing", "reviewing_design"]),
    ("media", &["listening", "watching"]),
    ("admin", &["file_management", "system_settings", "terminal_admin"]),
];

/// The activities of a task, or an empty list for unknown tasks.
pub fn activities(task: &str) -> &'static [&'static str] {
    TAXONOMY.iter().find(|(t, _)| *t == task).map(|(_, a)| *a).unwrap_or(&[])
}

pub fn is_known_task(task: &str) -> bool {
    TAXONOMY.iter().any(|(t, _)| *t == task)
}

pub fn is_known_activity(task: &str, activity: &str) -> bool {
    activities(task).contains(&activity)
}

/// "writing_code" -> "writing code", for display.
pub fn human(label: &str) -> String {
    label.replace('_', " ")
}

/// Validates a label typed by a user: short, lowercase words.
pub fn normalize_user_label(input: &str) -> Option<String> {
    let label: String = input
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_whitespace() || c == '-' { '_' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let label = label.trim_matches('_').to_string();
    (!label.is_empty() && label.chars().count() <= 40).then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_matches_known_tasks() {
        for task in crate::model::KNOWN_TASKS {
            assert!(!activities(task).is_empty(), "{task} has no activities");
        }
        assert!(is_known_activity("coding", "debugging"));
        assert!(!is_known_activity("coding", "chatting"));
        assert_eq!(human("writing_code"), "writing code");
        assert_eq!(normalize_user_label("  Grant Proposal "), Some("grant_proposal".into()));
        assert_eq!(normalize_user_label("!!!"), None);
    }
}

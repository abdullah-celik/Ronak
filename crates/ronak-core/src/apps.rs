//! Maps application ids (and sometimes titles) to broad categories.
//!
//! This is deliberately a plain table: easy to read, easy to extend with a
//! pull request, and good enough for the heuristic intent engine. A learned
//! classifier can replace it later without changing anything else.

use crate::model::AppCategory;

/// Substring patterns matched against the lowercased app id. First match wins,
/// so more specific patterns come first.
const TABLE: &[(&str, AppCategory)] = &[
    // Meetings before browsers/chat so "teams" and "zoom" win.
    ("zoom", AppCategory::Meeting),
    ("teams", AppCategory::Meeting),
    ("webex", AppCategory::Meeting),
    ("jitsi", AppCategory::Meeting),
    // Editors and IDEs.
    ("code", AppCategory::Editor), // code, code-oss, vscodium ("codium")
    ("codium", AppCategory::Editor),
    ("cursor", AppCategory::Editor),
    ("zed", AppCategory::Editor),
    ("jetbrains", AppCategory::Editor),
    ("sublime", AppCategory::Editor),
    ("emacs", AppCategory::Editor),
    ("neovide", AppCategory::Editor),
    ("texteditor", AppCategory::Editor),
    ("gedit", AppCategory::Editor),
    ("kate", AppCategory::Editor),
    ("builder", AppCategory::Editor),
    // Terminals.
    ("alacritty", AppCategory::Terminal),
    ("terminal", AppCategory::Terminal),
    ("ptyxis", AppCategory::Terminal),
    ("console", AppCategory::Terminal),
    ("kitty", AppCategory::Terminal),
    ("wezterm", AppCategory::Terminal),
    ("ghostty", AppCategory::Terminal),
    ("foot", AppCategory::Terminal),
    ("konsole", AppCategory::Terminal),
    ("tilix", AppCategory::Terminal),
    // Mail before generic chat.
    ("thunderbird", AppCategory::Mail),
    ("evolution", AppCategory::Mail),
    ("geary", AppCategory::Mail),
    ("hey.com", AppCategory::Mail),
    // Communication.
    ("slack", AppCategory::Communication),
    ("discord", AppCategory::Communication),
    ("signal", AppCategory::Communication),
    ("telegram", AppCategory::Communication),
    ("element", AppCategory::Communication),
    ("whatsapp", AppCategory::Communication),
    ("fractal", AppCategory::Communication),
    ("basecamp", AppCategory::Communication),
    // Browsers.
    ("firefox", AppCategory::Browser),
    ("chrome", AppCategory::Browser),
    ("chromium", AppCategory::Browser),
    ("brave", AppCategory::Browser),
    ("microsoft-edge", AppCategory::Browser),
    ("epiphany", AppCategory::Browser),
    ("vivaldi", AppCategory::Browser),
    ("librewolf", AppCategory::Browser),
    ("zen-browser", AppCategory::Browser),
    ("zen_browser", AppCategory::Browser),
    // Writing and office.
    ("libreoffice", AppCategory::Documents),
    ("obsidian", AppCategory::Documents),
    ("typora", AppCategory::Documents),
    ("notion", AppCategory::Documents),
    ("logseq", AppCategory::Documents),
    ("joplin", AppCategory::Documents),
    ("onlyoffice", AppCategory::Documents),
    // Readers.
    ("evince", AppCategory::Reader),
    ("papers", AppCategory::Reader),
    ("okular", AppCategory::Reader),
    ("zathura", AppCategory::Reader),
    ("foliate", AppCategory::Reader),
    ("xournal", AppCategory::Reader),
    // Media.
    ("spotify", AppCategory::Media),
    ("vlc", AppCategory::Media),
    ("mpv", AppCategory::Media),
    ("totem", AppCategory::Media),
    ("showtime", AppCategory::Media),
    ("rhythmbox", AppCategory::Media),
    ("celluloid", AppCategory::Media),
    // Design.
    ("gimp", AppCategory::Design),
    ("inkscape", AppCategory::Design),
    ("krita", AppCategory::Design),
    ("blender", AppCategory::Design),
    ("figma", AppCategory::Design),
    ("pinta", AppCategory::Design),
    // Files and system.
    ("nautilus", AppCategory::Files),
    ("dolphin", AppCategory::Files),
    ("thunar", AppCategory::Files),
    ("settings", AppCategory::System),
    ("control-center", AppCategory::System),
    ("systemmonitor", AppCategory::System),
    ("system-monitor", AppCategory::System),
    ("extensions", AppCategory::System),
];

/// Title keywords that override a browser's category (e.g. a video call in a tab).
const BROWSER_TITLE_HINTS: &[(&str, AppCategory)] = &[
    ("meet -", AppCategory::Meeting),
    ("google meet", AppCategory::Meeting),
    ("zoom meeting", AppCategory::Meeting),
    ("microsoft teams", AppCategory::Meeting),
    ("gmail", AppCategory::Mail),
    ("outlook", AppCategory::Mail),
    ("hey |", AppCategory::Mail),
    ("slack", AppCategory::Communication),
    ("whatsapp", AppCategory::Communication),
    ("discord", AppCategory::Communication),
    ("youtube", AppCategory::Media),
    ("spotify", AppCategory::Media),
    ("google docs", AppCategory::Documents),
    ("google sheets", AppCategory::Documents),
    ("figma", AppCategory::Design),
];

/// Title keywords suggesting developer documentation or tooling in a browser.
const DEV_TITLE_HINTS: &[&str] = &[
    "github",
    "gitlab",
    "stack overflow",
    "docs.rs",
    "crates.io",
    "mdn",
    "documentation",
    " docs",
    "api reference",
    "rust-lang",
    "python.org",
    "npm",
    "localhost",
    "127.0.0.1",
    "pull request",
];

/// Category of an app from its id alone.
pub fn categorize_id(app_id: &str) -> AppCategory {
    let id = app_id.to_ascii_lowercase();
    TABLE.iter().find(|(pattern, _)| id.contains(pattern)).map(|(_, c)| *c).unwrap_or(AppCategory::Other)
}

/// Category of a window from its app id and, for browsers, its title.
pub fn categorize(app_id: &str, title: Option<&str>) -> AppCategory {
    let base = categorize_id(app_id);
    if base == AppCategory::Browser
        && let Some(t) = title
    {
        let t = t.to_lowercase();
        if let Some((_, c)) = BROWSER_TITLE_HINTS.iter().find(|(k, _)| t.contains(k)) {
            return *c;
        }
    }
    base
}

/// True when a browser (or terminal) title looks like developer work.
pub fn looks_like_dev(title: Option<&str>) -> bool {
    match title {
        Some(t) => {
            let t = t.to_lowercase();
            DEV_TITLE_HINTS.iter().any(|k| t.contains(k))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_ids() {
        assert_eq!(categorize_id("Alacritty"), AppCategory::Terminal);
        assert_eq!(categorize_id("code"), AppCategory::Editor);
        assert_eq!(categorize_id("org.mozilla.firefox"), AppCategory::Browser);
        assert_eq!(categorize_id("org.gnome.Nautilus"), AppCategory::Files);
        assert_eq!(categorize_id("us.zoom.Zoom"), AppCategory::Meeting);
        assert_eq!(categorize_id("com.slack.Slack"), AppCategory::Communication);
        assert_eq!(categorize_id("md.obsidian.Obsidian"), AppCategory::Documents);
        assert_eq!(categorize_id("something-unheard-of"), AppCategory::Other);
    }

    #[test]
    fn browser_titles_refine_category() {
        assert_eq!(categorize("firefox", Some("Weekly sync - Google Meet")), AppCategory::Meeting);
        assert_eq!(categorize("google-chrome", Some("Inbox (3) - Gmail")), AppCategory::Mail);
        assert_eq!(categorize("firefox", Some("Rust docs")), AppCategory::Browser);
        assert!(looks_like_dev(Some("tokio - docs.rs")));
        assert!(!looks_like_dev(Some("Recipes for dinner")));
    }
}

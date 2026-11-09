//! Surfaces: interfaces Ronak draws itself, from a catalog of components.
//!
//! The principle: **anyone may compose, only the catalog may render.** A
//! surface is a flat list of components that reference each other by id
//! (the same shape as Google's open A2UI format, so it can map onto it). A
//! composer (today the deterministic [`RulesComposer`]; later an LLM; already
//! MCP agents through proposals) produces surfaces, and [`validate`] checks
//! every one of them against hard limits before anything is drawn. Buttons
//! can only trigger actions from a fixed allowlist, so a composed surface can
//! never do something the user could not already do through Ronak.
//!
//! Mirrors `schema/surface.schema.json`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::*;
use crate::protocol::SuggestionView;
use crate::taxonomy;

pub const MAX_COMPONENTS: usize = 200;
pub const MAX_DEPTH: usize = 10;
pub const MAX_TEXT: usize = 600;
pub const MAX_LABEL: usize = 80;

/// Symbolic icons a surface may use (all ship with GNOME's Adwaita theme).
pub const ICONS: &[&str] = &[
    "applications-system-symbolic",
    "audio-x-generic-symbolic",
    "dialog-information-symbolic",
    "document-edit-symbolic",
    "edit-undo-symbolic",
    "emblem-ok-symbolic",
    "folder-symbolic",
    "help-about-symbolic",
    "mail-unread-symbolic",
    "media-playback-pause-symbolic",
    "media-playback-start-symbolic",
    "media-skip-backward-symbolic",
    "media-skip-forward-symbolic",
    "preferences-system-notifications-symbolic",
    "starred-symbolic",
    "system-run-symbolic",
    "text-editor-symbolic",
    "user-available-symbolic",
    "utilities-terminal-symbolic",
    "view-grid-symbolic",
    "web-browser-symbolic",
    "window-close-symbolic",
    "x-office-calendar-symbolic",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    IntentPanel,
    ContextGlances,
    Proposal,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposerKind {
    Rules,
    Llm,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    pub schema_version: String,
    pub id: String,
    pub kind: SurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub composer: ComposerKind,
    /// For proposals: which agent proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub created_at: DateTime<Utc>,
    pub root: String,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStyle {
    #[default]
    Body,
    Caption,
    Heading,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Success,
    Warning,
}

/// The component catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Column {
        children: Vec<String>,
    },
    Row {
        children: Vec<String>,
    },
    Card {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        children: Vec<String>,
    },
    Text {
        text: String,
        #[serde(default)]
        style: TextStyle,
    },
    Badge {
        text: String,
        #[serde(default)]
        tone: Tone,
    },
    /// A bar from 0 to 1, e.g. confidence.
    Meter {
        value: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Button {
        label: String,
        action: UiAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// A set of one-tap choices.
    Chips {
        options: Vec<Chip>,
    },
    /// A single-line text field; on submit, its text is added to the action's
    /// args as "label".
    Entry {
        placeholder: String,
        action: UiAction,
    },
    Icon {
        name: String,
    },
    Divider,
}

impl Body {
    fn children(&self) -> &[String] {
        match self {
            Body::Column { children } | Body::Row { children } | Body::Card { children, .. } => children,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chip {
    pub label: String,
    pub action: UiAction,
    #[serde(default)]
    pub selected: bool,
}

/// Everything a surface is allowed to make Ronak do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionName {
    CorrectIntent,
    AcceptSuggestion,
    DismissSuggestion,
    Undo,
    ToggleObserving,
    MediaPlayPause,
    MediaNext,
    MediaPrevious,
    FocusWindow,
    AcceptProposal,
    DismissProposal,
    ClosePanel,
    /// Switch on accessibility so an app that hides its insides exposes them.
    EnableAccessibility,
    /// Do not ask again for this app.
    SkipAccessibility,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiAction {
    pub name: ActionName,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
}

impl UiAction {
    pub fn new(name: ActionName) -> Self {
        Self { name, args: Map::new() }
    }
    pub fn arg(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.args.insert(key.into(), value.into());
        self
    }
    pub fn str_arg(&self, key: &str) -> Option<&str> {
        self.args.get(key).and_then(Value::as_str)
    }
}

/// Parses and validates a surface from untrusted JSON (an agent or a model).
pub fn parse(json: &Value) -> Result<Surface, Vec<String>> {
    let surface: Surface =
        serde_json::from_value(json.clone()).map_err(|e| vec![format!("not a valid surface: {e}")])?;
    validate(&surface)?;
    Ok(surface)
}

/// Checks a surface against every rule of the catalog. All problems are
/// reported at once so a model can fix them in one retry.
pub fn validate(s: &Surface) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if !SUPPORTED_SCHEMA_VERSIONS.contains(&s.schema_version.as_str()) {
        errors.push(format!("unsupported schema_version {}", s.schema_version));
    }
    if s.components.is_empty() || s.components.len() > MAX_COMPONENTS {
        errors.push(format!("a surface needs 1 to {MAX_COMPONENTS} components, got {}", s.components.len()));
    }
    let mut by_id: HashMap<&str, &Component> = HashMap::new();
    for c in &s.components {
        let valid_id = !c.id.is_empty()
            && c.id.len() <= 64
            && c.id.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-');
        if !valid_id {
            errors.push(format!("invalid component id {:?} (use a-z, 0-9, _ and -)", c.id));
        }
        if by_id.insert(c.id.as_str(), c).is_some() {
            errors.push(format!("duplicate component id {:?}", c.id));
        }
        check_body(&c.id, &c.body, &mut errors);
    }
    if !by_id.contains_key(s.root.as_str()) {
        errors.push(format!("root {:?} is not a component", s.root));
        return Err(errors);
    }

    // Walk the tree from the root: every child must exist, have one parent,
    // sit within the depth limit, and every component must be reachable.
    let mut parent_count: HashMap<&str, usize> = HashMap::new();
    let mut reached: HashSet<&str> = HashSet::new();
    let mut stack: Vec<(&str, usize, Vec<&str>)> = vec![(s.root.as_str(), 1, vec![])];
    while let Some((id, depth, path)) = stack.pop() {
        if path.contains(&id) {
            errors.push(format!("cycle through {id:?}"));
            continue;
        }
        if depth > MAX_DEPTH {
            errors.push(format!("{id:?} is nested deeper than {MAX_DEPTH}"));
            continue;
        }
        reached.insert(id);
        let Some(c) = by_id.get(id) else { continue };
        for child in c.body.children() {
            if !by_id.contains_key(child.as_str()) {
                errors.push(format!("{id:?} refers to missing child {child:?}"));
                continue;
            }
            *parent_count.entry(child.as_str()).or_default() += 1;
            let mut p = path.clone();
            p.push(id);
            stack.push((child.as_str(), depth + 1, p));
        }
    }
    for (child, n) in parent_count {
        if n > 1 {
            errors.push(format!("{child:?} has {n} parents; each component may appear once"));
        }
    }
    for c in &s.components {
        if !reached.contains(c.id.as_str()) {
            errors.push(format!("{:?} is not reachable from the root", c.id));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

fn check_text(id: &str, what: &str, text: &str, max: usize, errors: &mut Vec<String>) {
    let n = text.chars().count();
    if n == 0 || n > max {
        errors.push(format!("{id:?}: {what} must be 1 to {max} characters"));
    }
}

fn check_body(id: &str, body: &Body, errors: &mut Vec<String>) {
    match body {
        Body::Column { children } | Body::Row { children } => {
            if children.is_empty() {
                errors.push(format!("{id:?}: a container needs children"));
            }
        }
        Body::Card { title, children } => {
            if let Some(t) = title {
                check_text(id, "title", t, MAX_LABEL, errors);
            }
            if children.is_empty() {
                errors.push(format!("{id:?}: a card needs children"));
            }
        }
        Body::Text { text, .. } => check_text(id, "text", text, MAX_TEXT, errors),
        Body::Badge { text, .. } => check_text(id, "badge", text, 32, errors),
        Body::Meter { value, label } => {
            if !value.is_finite() || !(0.0..=1.0).contains(value) {
                errors.push(format!("{id:?}: meter value must be between 0 and 1"));
            }
            if let Some(l) = label {
                check_text(id, "label", l, MAX_LABEL, errors);
            }
        }
        Body::Button { label, action, icon } => {
            check_text(id, "label", label, MAX_LABEL, errors);
            check_action(id, action, errors);
            if let Some(i) = icon {
                check_icon(id, i, errors);
            }
        }
        Body::Chips { options } => {
            if options.is_empty() || options.len() > 12 {
                errors.push(format!("{id:?}: chips need 1 to 12 options"));
            }
            for o in options {
                check_text(id, "chip label", &o.label, 40, errors);
                check_action(id, &o.action, errors);
            }
        }
        Body::Entry { placeholder, action } => {
            check_text(id, "placeholder", placeholder, MAX_LABEL, errors);
            if action.name != ActionName::CorrectIntent {
                errors.push(format!("{id:?}: an entry may only correct the intent"));
            }
        }
        Body::Icon { name } => check_icon(id, name, errors),
        Body::Divider => {}
    }
}

fn check_icon(id: &str, name: &str, errors: &mut Vec<String>) {
    if !ICONS.contains(&name) {
        errors.push(format!("{id:?}: icon {name:?} is not in the catalog"));
    }
}

fn check_action(id: &str, a: &UiAction, errors: &mut Vec<String>) {
    let allowed: &[&str] = match a.name {
        ActionName::CorrectIntent => &["label", "activity", "project"],
        ActionName::FocusWindow => &["window_id"],
        ActionName::AcceptProposal | ActionName::DismissProposal => &["proposal_id"],
        ActionName::MediaPlayPause | ActionName::MediaNext | ActionName::MediaPrevious => &["capability_id"],
        ActionName::EnableAccessibility | ActionName::SkipAccessibility => &["app_id"],
        _ => &[],
    };
    for (k, v) in &a.args {
        if !allowed.contains(&k.as_str()) {
            errors.push(format!("{id:?}: action {:?} does not take {k:?}", a.name));
        } else if !v.is_string() || v.as_str().is_some_and(|s| s.chars().count() > MAX_LABEL) {
            errors.push(format!("{id:?}: argument {k:?} must be a short string"));
        }
    }
    let required: &[&str] = match a.name {
        ActionName::FocusWindow => &["window_id"],
        ActionName::AcceptProposal | ActionName::DismissProposal => &["proposal_id"],
        ActionName::EnableAccessibility | ActionName::SkipAccessibility => &["app_id"],
        _ => &[],
    };
    for r in required {
        if !a.args.contains_key(*r) {
            errors.push(format!("{id:?}: action {:?} needs {r:?}", a.name));
        }
    }
}

// ----- composing ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelStyle {
    /// Docked to the right edge; layouts leave room for it.
    #[default]
    SidePanel,
    /// A small card near the top bar.
    FloatingCard,
}

/// Everything a composer may look at.
#[derive(Debug, Clone, Copy)]
pub struct ComposeInput<'a> {
    pub task: Option<&'a Task>,
    pub capabilities: &'a [Capability],
    pub suggestion: Option<&'a SuggestionView>,
    pub observing: bool,
    pub undo_available: bool,
    /// An app that hides its insides, and whether to ask about it: (app id, app name).
    pub accessibility_question: Option<(&'a str, &'a str)>,
    pub now: DateTime<Utc>,
}

/// Anything that turns context into surfaces. Output is always validated by
/// the caller; invalid surfaces are dropped and the rules composer's are used.
pub trait Composer: Send + Sync {
    fn kind(&self) -> ComposerKind;
    fn compose(&self, input: &ComposeInput) -> Vec<Surface>;
}

/// Small builder that keeps ids unique and the tree well formed.
struct Builder {
    components: Vec<Component>,
}

impl Builder {
    fn new() -> Self {
        Self { components: vec![] }
    }
    fn add(&mut self, id: impl Into<String>, body: Body) -> String {
        let id = id.into();
        self.components.push(Component { id: id.clone(), body });
        id
    }
    fn text(&mut self, id: &str, text: impl Into<String>, style: TextStyle) -> String {
        let mut t: String = text.into();
        if t.chars().count() > MAX_TEXT {
            t = t.chars().take(MAX_TEXT - 1).collect::<String>() + "…";
        }
        self.add(id, Body::Text { text: t, style })
    }
    fn finish(self, id: &str, kind: SurfaceKind, title: &str, root: String, now: DateTime<Utc>) -> Surface {
        Surface {
            schema_version: crate::model::SCHEMA_VERSION.into(),
            id: id.into(),
            kind,
            title: Some(title.into()),
            composer: ComposerKind::Rules,
            source: None,
            created_at: now,
            root,
            components: self.components,
        }
    }
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { s.chars().take(max - 1).collect::<String>() + "…" }
}

/// Deterministic templates per intent. The baseline that LLM composers must
/// match on stability before they are allowed to replace it.
#[derive(Debug, Default, Clone, Copy)]
pub struct RulesComposer;

impl Composer for RulesComposer {
    fn kind(&self) -> ComposerKind {
        ComposerKind::Rules
    }

    fn compose(&self, input: &ComposeInput) -> Vec<Surface> {
        let mut out = vec![intent_panel(input)];
        if let Some(g) = context_glances(input) {
            out.push(g);
        }
        out
    }
}

fn intent_panel(input: &ComposeInput) -> Surface {
    let mut b = Builder::new();
    let mut root = vec![];

    // What Ronak believes.
    let mut about = vec![];
    match input.task {
        Some(task) => {
            let mut line = taxonomy::human(&task.label);
            if let Some(a) = &task.activity {
                line = format!("{line} · {}", taxonomy::human(a));
            }
            about.push(b.text("task", capitalize(&line), TextStyle::Title));
            if let Some(p) = &task.project {
                about.push(b.text("project", format!("Project: {}", short(p, 60)), TextStyle::Caption));
            }
            if task.source == TaskSource::User {
                about.push(b.add("certainty", Body::Badge { text: "you said so".into(), tone: Tone::Accent }));
            } else {
                let pct = (task.confidence * 100.0).round() as i32;
                about.push(b.add(
                    "certainty",
                    Body::Meter { value: task.confidence.clamp(0.0, 1.0), label: Some(format!("{pct}% sure")) },
                ));
            }
            let reasons: Vec<&Evidence> = task.evidence.iter().filter(|e| e.detail.is_some()).take(4).collect();
            if !reasons.is_empty() {
                let mut why = vec![b.text("why-title", "Why I think so", TextStyle::Heading)];
                for (i, e) in reasons.iter().enumerate() {
                    why.push(b.text(
                        &format!("why-{i}"),
                        capitalize(e.detail.as_deref().unwrap_or("")),
                        TextStyle::Caption,
                    ));
                }
                about.push(b.add("why", Body::Column { children: why }));
            }
        }
        None => {
            about.push(b.text("task", "Not sure yet", TextStyle::Title));
            about.push(b.text("hint", "Keep working; Ronak learns from what you focus on.", TextStyle::Caption));
        }
    }
    root.push(b.add("about", Body::Card { title: Some("What you're doing".into()), children: about }));

    // One-tap corrections: alternatives first, then sibling activities.
    let mut options: Vec<Chip> = vec![];
    let push = |label: &str, activity: Option<&str>, options: &mut Vec<Chip>| {
        let text = match activity {
            Some(a) if label == input.task.map(|t| t.label.as_str()).unwrap_or("") => taxonomy::human(a),
            Some(a) => format!("{} · {}", taxonomy::human(label), taxonomy::human(a)),
            None => taxonomy::human(label),
        };
        if options.len() >= 8 || options.iter().any(|o| o.label == text) {
            return;
        }
        let mut action = UiAction::new(ActionName::CorrectIntent).arg("label", label);
        if let Some(a) = activity {
            action = action.arg("activity", a);
        }
        options.push(Chip { label: short(&text, 40), action, selected: false });
    };
    if let Some(task) = input.task {
        for alt in &task.alternatives {
            push(&alt.label, alt.activity.as_deref(), &mut options);
        }
        for a in taxonomy::activities(&task.label) {
            if task.activity.as_deref() != Some(*a) {
                push(&task.label, Some(a), &mut options);
            }
        }
    }
    for t in KNOWN_TASKS {
        if input.task.map(|x| x.label.as_str()) != Some(*t) {
            push(t, None, &mut options);
        }
    }
    let chips = b.add("choices", Body::Chips { options });
    let entry = b.add(
        "own-label",
        Body::Entry {
            placeholder: "Or type what you're doing".into(),
            action: UiAction::new(ActionName::CorrectIntent),
        },
    );
    root.push(b.add("correct", Body::Card { title: Some("Not right? Tell me".into()), children: vec![chips, entry] }));

    // An app that does not share its insides yet: ask once.
    if let Some((app_id, app_name)) = input.accessibility_question {
        let text = b.text(
            "a11y-text",
            format!("{} does not share what is inside its window, so Ronak can only guess from its title. Switch on accessibility for apps like it? They may need a restart.", short(app_name, 40)),
            TextStyle::Caption,
        );
        let yes = b.add(
            "a11y-yes",
            Body::Button {
                label: "Switch on".into(),
                action: UiAction::new(ActionName::EnableAccessibility).arg("app_id", app_id),
                icon: None,
            },
        );
        let no = b.add(
            "a11y-no",
            Body::Button {
                label: "Not for this app".into(),
                action: UiAction::new(ActionName::SkipAccessibility).arg("app_id", app_id),
                icon: None,
            },
        );
        let row = b.add("a11y-buttons", Body::Row { children: vec![yes, no] });
        root.push(b.add("a11y", Body::Card { title: Some("See more inside apps?".into()), children: vec![text, row] }));
    }

    // The pending layout suggestion, if any.
    if let Some(s) = input.suggestion {
        let summary = b.text("suggestion-text", short(&s.summary, 200), TextStyle::Body);
        let apply = b.add(
            "apply",
            Body::Button {
                label: "Apply".into(),
                action: UiAction::new(ActionName::AcceptSuggestion),
                icon: Some("emblem-ok-symbolic".into()),
            },
        );
        let dismiss = b.add(
            "dismiss",
            Body::Button { label: "Not now".into(), action: UiAction::new(ActionName::DismissSuggestion), icon: None },
        );
        let buttons = b.add("suggestion-buttons", Body::Row { children: vec![apply, dismiss] });
        root.push(b.add(
            "suggestion",
            Body::Card { title: Some("Suggested layout".into()), children: vec![summary, buttons] },
        ));
    }

    // Footer controls.
    let mut footer = vec![];
    if input.undo_available {
        footer.push(b.add(
            "undo",
            Body::Button {
                label: "Undo".into(),
                action: UiAction::new(ActionName::Undo),
                icon: Some("edit-undo-symbolic".into()),
            },
        ));
    }
    footer.push(b.add(
        "observe",
        Body::Button {
            label: if input.observing { "Pause watching".into() } else { "Resume watching".into() },
            action: UiAction::new(ActionName::ToggleObserving),
            icon: None,
        },
    ));
    root.push(b.add("footer", Body::Row { children: footer }));

    let root = b.add("root", Body::Column { children: root });
    b.finish("intent-panel", SurfaceKind::IntentPanel, "Ronak", root, input.now)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn local_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&chrono::Local).format("%H:%M").to_string()
}

fn ago(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (now - t).num_minutes().max(0);
    match mins {
        0 => "just now".into(),
        m if m < 60 => format!("{m} min ago"),
        m => format!("{} h ago", m / 60),
    }
}

fn context_glances(input: &ComposeInput) -> Option<Surface> {
    let mut b = Builder::new();
    let mut cards = vec![];

    for (i, cap) in input.capabilities.iter().enumerate() {
        let n = i.to_string();
        if let Some(m) = &cap.media {
            let mut children = vec![];
            let what = match (&m.title, &m.artist) {
                (Some(t), Some(a)) => format!("{} — {}", short(t, 60), short(a, 40)),
                (Some(t), None) => short(t, 80),
                _ => "Nothing playing".into(),
            };
            children.push(b.text(&format!("media-what-{n}"), what, TextStyle::Body));
            let status = match m.status {
                PlaybackStatus::Playing => ("playing", Tone::Success),
                PlaybackStatus::Paused => ("paused", Tone::Neutral),
                PlaybackStatus::Stopped => ("stopped", Tone::Neutral),
            };
            children.push(b.add(format!("media-status-{n}"), Body::Badge { text: status.0.into(), tone: status.1 }));
            if cap.access == Access::Act {
                let control = |b: &mut Builder, id: &str, name: ActionName, icon: &str, label: &str| {
                    b.add(
                        format!("{id}-{n}"),
                        Body::Button {
                            label: label.into(),
                            action: UiAction::new(name).arg("capability_id", cap.id.as_str()),
                            icon: Some(icon.into()),
                        },
                    )
                };
                let play_icon = if m.status == PlaybackStatus::Playing {
                    "media-playback-pause-symbolic"
                } else {
                    "media-playback-start-symbolic"
                };
                let prev = control(
                    &mut b,
                    "media-prev",
                    ActionName::MediaPrevious,
                    "media-skip-backward-symbolic",
                    "Previous",
                );
                let play = control(&mut b, "media-play", ActionName::MediaPlayPause, play_icon, "Play/pause");
                let next = control(&mut b, "media-next", ActionName::MediaNext, "media-skip-forward-symbolic", "Next");
                children.push(b.add(format!("media-controls-{n}"), Body::Row { children: vec![prev, play, next] }));
            }
            cards.push(b.add(format!("media-{n}"), Body::Card { title: Some(short(&cap.name, 40)), children }));
        }
        if let Some(c) = &cap.calendar {
            let mut children = vec![];
            for (j, e) in c.current.iter().take(2).enumerate() {
                let what = e.summary.as_deref().map(|s| short(s, 60)).unwrap_or_else(|| "An event".into());
                children.push(b.text(
                    &format!("cal-now-{n}-{j}"),
                    format!("Now: {what} (until {})", local_time(e.end)),
                    TextStyle::Body,
                ));
            }
            for (j, e) in c.upcoming.iter().take(3).enumerate() {
                let what = e.summary.as_deref().map(|s| short(s, 60)).unwrap_or_else(|| "An event".into());
                let when = if e.all_day { "All day".to_string() } else { local_time(e.start) };
                children.push(b.text(&format!("cal-next-{n}-{j}"), format!("{when}  {what}"), TextStyle::Caption));
            }
            if children.is_empty() {
                children.push(b.text(&format!("cal-empty-{n}"), "Nothing else today", TextStyle::Caption));
            }
            cards.push(b.add(format!("calendar-{n}"), Body::Card { title: Some("Calendar".into()), children }));
        }
        if let Some(ns) = &cap.notifications {
            let mut children = vec![];
            if ns.count_last_hour > 0 {
                children.push(b.add(
                    format!("notif-count-{n}"),
                    Body::Badge { text: format!("{} in the last hour", ns.count_last_hour), tone: Tone::Accent },
                ));
            }
            for (j, item) in ns.recent.iter().take(3).enumerate() {
                let line = match &item.title {
                    Some(t) => format!("{}: {}", short(&item.app, 30), short(t, 70)),
                    None => short(&item.app, 30),
                };
                children.push(b.text(&format!("notif-{n}-{j}"), line, TextStyle::Body));
                children.push(b.text(&format!("notif-ago-{n}-{j}"), ago(item.at, input.now), TextStyle::Caption));
            }
            if children.is_empty() {
                children.push(b.text(&format!("notif-empty-{n}"), "Nothing new", TextStyle::Caption));
            }
            cards.push(
                b.add(format!("notifications-{n}"), Body::Card { title: Some("Notifications".into()), children }),
            );
        }
        if let Some(m) = &cap.mcp {
            let text = if m.connected { format!("{} tools available", m.tools.len()) } else { "not connected".into() };
            let line = b.text(&format!("mcp-{n}-line"), text, TextStyle::Caption);
            cards.push(
                b.add(format!("mcp-{n}"), Body::Card { title: Some(short(&cap.name, 40)), children: vec![line] }),
            );
        }
    }
    if cards.is_empty() {
        return None;
    }
    let root = b.add("root", Body::Column { children: cards });
    Some(b.finish("context-glances", SurfaceKind::ContextGlances, "Context", root, input.now))
}

/// Wraps an agent's validated proposal with Accept and Dismiss controls.
/// Ids are prefixed so they cannot collide with the wrapper's.
pub fn wrap_proposal(mut proposal: Surface, proposal_id: &str, agent: &str) -> Surface {
    for c in &mut proposal.components {
        c.id = format!("p-{}", c.id);
        match &mut c.body {
            Body::Column { children } | Body::Row { children } | Body::Card { children, .. } => {
                for ch in children.iter_mut() {
                    *ch = format!("p-{ch}");
                }
            }
            _ => {}
        }
    }
    let inner_root = format!("p-{}", proposal.root);
    let mut b = Builder { components: proposal.components };
    let accept = b.add(
        "proposal-accept",
        Body::Button {
            label: "Keep".into(),
            action: UiAction::new(ActionName::AcceptProposal).arg("proposal_id", proposal_id),
            icon: Some("emblem-ok-symbolic".into()),
        },
    );
    let dismiss = b.add(
        "proposal-dismiss",
        Body::Button {
            label: "Dismiss".into(),
            action: UiAction::new(ActionName::DismissProposal).arg("proposal_id", proposal_id),
            icon: None,
        },
    );
    let row = b.add("proposal-buttons", Body::Row { children: vec![accept, dismiss] });
    let card = b.add(
        "proposal",
        Body::Card { title: Some(short(&format!("Proposed by {agent}"), MAX_LABEL)), children: vec![inner_root, row] },
    );
    let title = proposal.title.clone().unwrap_or_else(|| "Proposal".into());
    let mut s = b.finish(
        &format!("proposal-{proposal_id}"),
        SurfaceKind::Proposal,
        &short(&title, MAX_LABEL),
        card,
        proposal.created_at,
    );
    s.composer = ComposerKind::Agent;
    s.source = Some(agent.into());
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T14:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn task() -> Task {
        Task {
            label: "coding".into(),
            confidence: 0.82,
            activity: Some("debugging".into()),
            project: Some("ronak".into()),
            evidence: vec![Evidence {
                signal: "activity".into(),
                detail: Some("the terminal shows errors".into()),
                weight: 0.7,
            }],
            alternatives: vec![Alternative {
                label: "coding".into(),
                activity: Some("testing".into()),
                confidence: 0.4,
            }],
            ..Task::default()
        }
    }

    fn caps() -> Vec<Capability> {
        let base = |id: &str, kind| Capability {
            id: id.into(),
            kind,
            provider: "test".into(),
            name: "Spotify".into(),
            access: Access::Act,
            actions: vec![],
            media: None,
            calendar: None,
            notifications: None,
            mcp: None,
        };
        let mut media = base("media:spotify", CapabilityKind::Media);
        media.media = Some(MediaState {
            status: PlaybackStatus::Playing,
            title: Some("Song".into()),
            artist: Some("Band".into()),
        });
        let mut cal = base("calendar", CapabilityKind::Calendar);
        cal.calendar = Some(CalendarState {
            current: vec![],
            upcoming: vec![CalendarEvent {
                summary: Some("Review".into()),
                start: now(),
                end: now(),
                location: None,
                all_day: false,
            }],
        });
        let mut notes = base("notifications", CapabilityKind::Notifications);
        notes.notifications = Some(NotificationsState {
            recent: vec![NotificationInfo {
                app: "Slack".into(),
                title: Some("Ana".into()),
                body: None,
                at: now(),
                urgency: Urgency::Normal,
            }],
            count_last_hour: 4,
        });
        vec![media, cal, notes]
    }

    #[test]
    fn rules_composer_output_is_valid() {
        let t = task();
        let c = caps();
        let suggestion =
            SuggestionView { id: "s1".into(), task_label: "coding".into(), summary: "coding: Code main".into() };
        for (task, sugg) in [(Some(&t), Some(&suggestion)), (None, None)] {
            let input = ComposeInput {
                task,
                capabilities: &c,
                suggestion: sugg,
                observing: true,
                undo_available: true,
                accessibility_question: Some(("code", "Code")),
                now: now(),
            };
            let surfaces = RulesComposer.compose(&input);
            assert_eq!(surfaces.len(), 2);
            for s in &surfaces {
                validate(s).unwrap_or_else(|e| panic!("{}: {e:?}", s.id));
                // And it survives a JSON round trip through the untrusted path.
                assert_eq!(&parse(&serde_json::to_value(s).unwrap()).unwrap(), s);
            }
        }
    }

    #[test]
    fn corrections_offer_alternatives_then_siblings() {
        let t = task();
        let input = ComposeInput {
            task: Some(&t),
            capabilities: &[],
            suggestion: None,
            observing: true,
            undo_available: false,
            accessibility_question: None,
            now: now(),
        };
        let panel = &RulesComposer.compose(&input)[0];
        let chips = panel
            .components
            .iter()
            .find_map(|c| match &c.body {
                Body::Chips { options } => Some(options),
                _ => None,
            })
            .unwrap();
        assert_eq!(chips[0].label, "testing");
        assert_eq!(chips[0].action.str_arg("activity"), Some("testing"));
        assert!(!chips.iter().any(|c| c.label == "debugging"), "the current activity is not offered");
    }

    #[test]
    fn validator_rejects_bad_surfaces() {
        let base = |components: Value, root: &str| {
            json!({ "schema_version": "0.2", "id": "x", "kind": "custom", "composer": "llm",
                    "created_at": "2026-11-09T14:00:00Z", "root": root, "components": components })
        };
        let cases = [
            (base(json!([{ "id": "a", "type": "column", "children": ["a"] }]), "a"), "cycle"),
            (base(json!([{ "id": "a", "type": "column", "children": ["b"] }]), "a"), "missing child"),
            (
                base(
                    json!([{ "id": "a", "type": "text", "text": "hi" }, { "id": "b", "type": "text", "text": "x" }]),
                    "a",
                ),
                "not reachable",
            ),
            (
                base(json!([{ "id": "a", "type": "button", "label": "x", "action": { "name": "rm_rf" } }]), "a"),
                "not a valid surface",
            ),
            (
                base(json!([{ "id": "a", "type": "button", "label": "x", "action": { "name": "focus_window" } }]), "a"),
                "needs",
            ),
            (base(json!([{ "id": "a", "type": "icon", "name": "../../etc/passwd" }]), "a"), "not in the catalog"),
            (base(json!([{ "id": "A B", "type": "divider" }]), "A B"), "invalid component id"),
            (base(json!([{ "id": "a", "type": "meter", "value": 3.0 }]), "a"), "between 0 and 1"),
            (base(json!([{ "id": "a", "type": "script", "code": "alert(1)" }]), "a"), "not a valid surface"),
        ];
        for (json, expected) in cases {
            let errs = parse(&json).expect_err(expected);
            assert!(errs.iter().any(|e| e.contains(expected)), "expected {expected:?} in {errs:?}");
        }
    }

    #[test]
    fn proposals_are_wrapped_and_stay_valid() {
        let agent = json!({ "schema_version": "0.2", "id": "focus", "kind": "custom", "composer": "agent", "title": "Focus",
            "created_at": "2026-11-09T14:00:00Z", "root": "root",
            "components": [ { "id": "root", "type": "column", "children": ["t"] }, { "id": "t", "type": "text", "text": "Close Slack for an hour?" } ] });
        let s = parse(&agent).unwrap();
        let wrapped = wrap_proposal(s, "p7", "claude-code");
        validate(&wrapped).unwrap();
        assert_eq!(wrapped.kind, SurfaceKind::Proposal);
        assert_eq!(wrapped.source.as_deref(), Some("claude-code"));
    }
}

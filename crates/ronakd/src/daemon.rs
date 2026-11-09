//! The daemon's main loop: one task that owns all state.
//!
//! Everything that can happen arrives as a message on a channel: desktop
//! events from the adapter, requests from the control socket, answers from
//! the LLM tier, and timer ticks. Because a single task handles them one at a
//! time, no locks are needed anywhere in the daemon (the "actor" pattern).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use chrono::{DateTime, Duration, Utc};
use ronak_atspi::ReadOptions;
use ronak_caps::media::MediaCommand;
use ronak_core::actions::UndoStack;
use ronak_core::activity::{self, Signals};
use ronak_core::adapter::{self, AdapterRequest};
use ronak_core::config::{Config, SuggestMode};
use ronak_core::intent::{HeuristicIntent, IntentModel, TaskTracker, TrackerUpdate};
use ronak_core::model::{
    Access, ActivityState, Capability, CapabilityKind, DetailLevel, Element, NotificationInfo, NotificationsState,
    Rect, Role, Snapshot, Task, TaskMember, TaskSource,
};
use ronak_core::privacy::{self, ExclusionMode, ExclusionRule};
use ronak_core::protocol::{Action, AdapterCommand, PlatformEvent, UserAction};
use ronak_core::state::{Change, DesktopState};
use ronak_core::suggest::{self, Policy, Suggestion};
use ronak_core::surface::{self, ActionName, ComposeInput, Composer, RulesComposer, Surface, UiAction};
use ronak_core::taxonomy;
use ronak_store::Store;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::llm::{LlmAnswer, LlmTier};
use crate::sensors::{SensorUpdate, Sensors};

/// How long a notification stays in the context.
const NOTIFICATION_MEMORY: Duration = Duration::hours(1);

/// A request from the control socket (CLI and MCP bridge).
#[derive(Debug, Clone)]
pub enum ControlRequest {
    Status,
    Context {
        detail: DetailLevel,
    },
    Suggestion {
        detail: DetailLevel,
    },
    Apply,
    Dismiss,
    Undo,
    SetObserving(bool),
    Hardware,
    /// "That's not what I'm doing" (schema 0.2).
    Correct {
        label: String,
        activity: Option<String>,
        project: Option<String>,
    },
    /// The panel subscribes to surface updates on this channel.
    Subscribe(mpsc::Sender<Value>),
    /// A button, chip or entry in a surface was used.
    SurfaceAction {
        action: UiAction,
        text: Option<String>,
    },
    /// Show, hide (or toggle, with `None`) the panel, or switch its style.
    Panel {
        visible: Option<bool>,
        style: Option<surface::PanelStyle>,
    },
    /// An MCP agent proposes a layout: app names or window ids.
    ProposeLayout {
        client: String,
        primary: String,
        references: Vec<String>,
        note: Option<String>,
    },
    /// An MCP agent proposes a surface for the panel.
    ProposeSurface {
        client: String,
        surface: Value,
    },
    /// Condensed patterns from the last `hours`.
    Patterns {
        hours: i64,
    },
    /// Call a tool on an MCP server you configured (needs `act` access).
    McpCall {
        server: String,
        tool: String,
        args: Value,
    },
}

pub struct ControlMsg {
    pub request: ControlRequest,
    pub reply: oneshot::Sender<Result<Value, String>>,
}

/// A layout Ronak applied, remembered so later manual moves can be recorded
/// as "overridden" feedback.
struct Applied {
    rect: Rect,
    at: DateTime<Utc>,
    suggestion_id: String,
}

pub struct Daemon {
    cfg: Config,
    exclusions: Vec<ExclusionRule>,
    state: DesktopState,
    store: Store,
    intent: HeuristicIntent,
    tracker: TaskTracker,
    undo: UndoStack,
    suggestion: Option<Suggestion>,
    suggestion_seq: u64,
    cooldown_until: Option<DateTime<Utc>>,
    applied: HashMap<String, Applied>,
    dnd_set_by_ronak: bool,
    commands: mpsc::Sender<AdapterRequest>,
    adapter_name: &'static str,
    dirty: bool,
    last_inferred: Option<DateTime<Utc>>,
    last_activity_state: Option<ActivityState>,
    hardware: Value,
    llm: Option<LlmTier>,
    llm_tx: mpsc::Sender<LlmAnswer>,
    started_at: DateTime<Utc>,
    // ----- schema 0.2 -----
    sensors: Sensors,
    /// Elements of the focused app, from the last accessibility read.
    elements: Vec<Element>,
    elements_window: Option<String>,
    element_read_at: Option<DateTime<Utc>>,
    element_in_flight: bool,
    media: Vec<Capability>,
    calendar: Option<Capability>,
    mcp: Vec<Capability>,
    notifications: VecDeque<NotificationInfo>,
    /// An app that hides its insides and was not asked about yet.
    a11y_question: Option<(String, String)>,
    proposals: BTreeMap<String, (Surface, bool)>,
    proposal_seq: u64,
    subscribers: Vec<mpsc::Sender<Value>>,
    last_published: Option<Value>,
    panel_visible: bool,
    lowered_units: HashSet<String>,
    last_digest: Option<DateTime<Utc>>,
}

impl Daemon {
    /// Wires the daemon to its store, adapter, LLM tier and sensors.
    #[allow(clippy::too_many_arguments)] // one constructor that connects all the channels
    pub fn new(
        cfg: Config,
        store: Store,
        commands: mpsc::Sender<AdapterRequest>,
        adapter_name: &'static str,
        hardware: Value,
        llm: Option<LlmTier>,
        llm_tx: mpsc::Sender<LlmAnswer>,
        sensors: Sensors,
    ) -> Self {
        let intent = HeuristicIntent {
            lookback: Duration::minutes(cfg.intent.lookback_minutes),
            half_life_secs: cfg.intent.half_life_secs,
            min_evidence_secs: cfg.intent.min_evidence_secs,
            ..HeuristicIntent::default()
        };
        Self {
            exclusions: cfg.observe.effective_exclusions(),
            tracker: TaskTracker::new(cfg.intent.min_confidence),
            cfg,
            state: DesktopState::new(),
            store,
            intent,
            undo: UndoStack::new(20),
            suggestion: None,
            suggestion_seq: 0,
            cooldown_until: None,
            applied: HashMap::new(),
            dnd_set_by_ronak: false,
            commands,
            adapter_name,
            dirty: false,
            last_inferred: None,
            last_activity_state: None,
            hardware,
            llm,
            llm_tx,
            started_at: Utc::now(),
            sensors,
            elements: vec![],
            elements_window: None,
            element_read_at: None,
            element_in_flight: false,
            media: vec![],
            calendar: None,
            mcp: vec![],
            notifications: VecDeque::new(),
            a11y_question: None,
            proposals: BTreeMap::new(),
            proposal_seq: 0,
            subscribers: vec![],
            last_published: None,
            panel_visible: false,
            lowered_units: HashSet::new(),
            last_digest: None,
        }
    }

    /// Shows the panel at start if configured, in the style you last chose.
    pub fn with_panel_visible(mut self, visible: bool) -> Self {
        self.panel_visible = visible;
        if let Some(style) =
            self.store.get_meta("panel.style").ok().flatten().and_then(|s| serde_json::from_str(&s).ok())
        {
            self.cfg.surfaces.style = style;
        }
        self
    }

    /// Runs until `shutdown` completes or every input closes.
    pub async fn run(
        mut self,
        mut events: mpsc::Receiver<PlatformEvent>,
        mut control: mpsc::Receiver<ControlMsg>,
        mut llm_rx: mpsc::Receiver<LlmAnswer>,
        mut sensor_rx: mpsc::Receiver<SensorUpdate>,
        shutdown: impl std::future::Future<Output = ()>,
    ) {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
        let mut prune = tokio::time::interval(std::time::Duration::from_secs(3600));
        tokio::pin!(shutdown);
        info!(adapter = self.adapter_name, "ronakd is running");
        loop {
            tokio::select! {
                Some(event) = events.recv() => self.handle_event(event, Utc::now()).await,
                Some(msg) = control.recv() => match msg.request {
                    ControlRequest::McpCall { server, tool, args } => self.spawn_mcp_call(server, tool, args, msg.reply),
                    request => {
                        let result = self.handle_control(request, Utc::now()).await;
                        let _ = msg.reply.send(result);
                    }
                },
                Some(answer) = llm_rx.recv() => self.handle_llm(answer, Utc::now()).await,
                Some(update) = sensor_rx.recv() => self.handle_sensor(update, Utc::now()).await,
                _ = tick.tick() => self.on_tick(Utc::now()).await,
                _ = prune.tick() => self.prune(Utc::now()),
                _ = &mut shutdown => break,
            }
        }
        self.close_task_span(Utc::now());
        self.restore_cpu();
        info!("ronakd stopped");
    }

    // ----- desktop events -------------------------------------------------

    pub async fn handle_event(&mut self, event: PlatformEvent, now: DateTime<Utc>) {
        let Some(event) = self.filter(event) else { return };
        let change = self.state.apply(&event, now);
        if self.state.session.observing {
            self.record(&event, &change, now);
        }
        match change {
            Change::Hello => {
                let cfg = self.adapter_config();
                adapter::send(&self.commands, AdapterCommand::Configure { config: cfg }).await;
                self.arrange_panel().await;
                self.dirty = true;
            }
            Change::Resync | Change::Windows | Change::Workspaces | Change::Monitors => self.dirty = true,
            Change::Focus { closed } => {
                if let (Some(span), true) = (closed, self.state.session.observing)
                    && let Some(app) = &span.app_id
                    && let Err(e) = self.store.add_focus_time(app, span.start, span.end)
                {
                    warn!("could not record focus time: {e}");
                }
                self.dirty = true;
                self.request_elements(now, true);
            }
            Change::Geometry { window_id } => {
                self.detect_override(&window_id, now);
                self.dirty = true;
            }
            Change::User(action) => {
                if let Err(e) = self.handle_user(action, now).await {
                    debug!("user action ignored: {e}");
                }
            }
            Change::Gone => {
                info!("adapter disconnected; waiting for it to come back");
                self.suggestion = None;
            }
            Change::Notification(n) => {
                if self.cfg.capabilities.notifications != Access::Off && self.state.session.observing {
                    self.notifications.push_front(n);
                    self.notifications.truncate(20);
                    self.dirty = true;
                    self.publish_surfaces(now).await;
                }
            }
            Change::Activity | Change::Session | Change::Nothing => {}
        }
    }

    /// Defense in depth: re-applies exclusion rules even though the adapter
    /// already did. A window that becomes excluded (say, a browser switching
    /// to private browsing) is reported as closed.
    fn filter(&self, event: PlatformEvent) -> Option<PlatformEvent> {
        let titles = self.cfg.observe.titles;
        let rules = &self.exclusions;
        Some(match event {
            PlatformEvent::WindowOpened { window } => {
                PlatformEvent::WindowOpened { window: privacy::apply_exclusions(rules, window, titles)? }
            }
            PlatformEvent::WindowChanged { window } => {
                let id = window.id.clone();
                match privacy::apply_exclusions(rules, window, titles) {
                    Some(window) => PlatformEvent::WindowChanged { window },
                    None => PlatformEvent::WindowClosed { window_id: id },
                }
            }
            PlatformEvent::Snapshot { monitors, workspaces, windows, focused_window, idle_ms, session } => {
                let windows: Vec<_> =
                    windows.into_iter().filter_map(|w| privacy::apply_exclusions(rules, w, titles)).collect();
                let focused_window = focused_window.filter(|id| windows.iter().any(|w| &w.id == id));
                PlatformEvent::Snapshot { monitors, workspaces, windows, focused_window, idle_ms, session }
            }
            PlatformEvent::FocusChanged { window_id: Some(id) } if !self.state.windows.contains_key(&id) => {
                // Focus moved to an ignored (or unknown) window.
                PlatformEvent::FocusChanged { window_id: None }
            }
            other => other,
        })
    }

    fn record(&mut self, event: &PlatformEvent, change: &Change, now: DateTime<Utc>) {
        let result = match event {
            PlatformEvent::WindowOpened { window } | PlatformEvent::WindowChanged { window } => {
                let kind = match (event, change) {
                    (PlatformEvent::WindowOpened { .. }, _) => "window_opened",
                    (_, Change::Geometry { .. }) => "window_moved",
                    _ => "window_changed",
                };
                self.store.record_event(
                    now,
                    kind,
                    Some(&window.id),
                    window.app_id(),
                    window.title.as_deref(),
                    &json!({ "rect": window.rect, "maximized": window.maximized, "minimized": window.minimized }),
                )
            }
            PlatformEvent::WindowClosed { window_id } => {
                self.store.record_event(now, "window_closed", Some(window_id), None, None, &Value::Null)
            }
            PlatformEvent::FocusChanged { window_id } => {
                let app = window_id
                    .as_ref()
                    .and_then(|id| self.state.windows.get(id))
                    .and_then(|w| w.app_id().map(str::to_owned));
                self.store.record_event(now, "focus", window_id.as_deref(), app.as_deref(), None, &Value::Null)
            }
            PlatformEvent::Activity { .. } => {
                let state = self.state.activity(now).state;
                if self.last_activity_state == Some(state) {
                    return;
                }
                self.last_activity_state = Some(state);
                self.store.record_event(now, "activity", None, None, None, &json!({ "state": state }))
            }
            PlatformEvent::Snapshot { windows, .. } => {
                self.store.record_event(now, "snapshot", None, None, None, &json!({ "windows": windows.len() }))
            }
            PlatformEvent::WorkspacesChanged { workspaces } => {
                self.store.record_event(now, "workspaces", None, None, None, &json!(workspaces))
            }
            PlatformEvent::MonitorsChanged { monitors } => {
                self.store.record_event(now, "monitors", None, None, None, &json!({ "count": monitors.len() }))
            }
            _ => return,
        };
        if let Err(e) = result {
            warn!("could not record event: {e}");
        }
    }

    /// A window Ronak placed was moved by hand soon after: useful feedback.
    fn detect_override(&mut self, window_id: &str, now: DateTime<Utc>) {
        let Some(applied) = self.applied.get(window_id) else { return };
        let age = now - applied.at;
        if age < Duration::seconds(2) {
            return; // our own move still settling
        }
        if age > Duration::minutes(2) {
            self.applied.remove(window_id);
            return;
        }
        let Some(window) = self.state.windows.get(window_id) else { return };
        if window.rect.max_edge_delta(&applied.rect) <= self.cfg.layout.stability_px {
            return;
        }
        let data =
            json!({ "window_id": window_id, "app": window.app_id(), "placed": applied.rect, "moved_to": window.rect });
        let sid = applied.suggestion_id.clone();
        self.applied.remove(window_id);
        let label = self.tracker.current().map(|t| t.label.clone());
        info!(window_id, "layout overridden by hand; noted as feedback");
        let _ = self.store.record_feedback(now, Some(&sid), "overridden", label.as_deref(), &data);
    }

    async fn handle_user(&mut self, action: UserAction, now: DateTime<Utc>) -> Result<Value, String> {
        match action {
            UserAction::AcceptSuggestion { suggestion_id } => self.apply_current(Some(&suggestion_id), now).await,
            UserAction::DismissSuggestion { suggestion_id } => self.dismiss(Some(&suggestion_id), now).await,
            UserAction::Undo => self.undo(now).await,
            UserAction::SetObserving { observing } => {
                info!(observing, "observation {}", if observing { "resumed" } else { "paused" });
                if !observing {
                    self.clear_suggestion().await;
                    self.elements.clear();
                    self.restore_cpu();
                }
                self.publish_surfaces(now).await;
                Ok(json!({ "observing": observing }))
            }
            UserAction::CorrectIntent { label, activity, project } => self.correct(label, activity, project, now).await,
            UserAction::TogglePanel => self.set_panel(None, now).await,
        }
    }

    // ----- intent and suggestions ----------------------------------------

    pub async fn on_tick(&mut self, now: DateTime<Utc>) {
        self.request_elements(now, false);
        if self.cooldown_until.is_some_and(|t| now >= t) {
            self.cooldown_until = None;
            self.dirty = true;
        }
        // Re-infer soon after something changed, and periodically anyway:
        // staying in one window for ten minutes is evidence too.
        let since_last = self.last_inferred.map(|t| now - t);
        let due = match since_last {
            None => self.dirty,
            Some(elapsed) => (self.dirty && elapsed >= Duration::seconds(1)) || elapsed >= Duration::seconds(10),
        };
        if !due || !self.state.is_connected() {
            return;
        }
        self.dirty = false;
        self.last_inferred = Some(now);
        if !self.state.session.observing {
            return;
        }
        let candidate = self.intent.infer(&self.state, now).map(|t| self.refine(t, now));
        self.maybe_ask_llm(candidate.as_ref(), now);
        let update = self.tracker.update(candidate, now);
        self.on_task_update(update, now).await;
    }

    /// Adds activity, project and evidence from elements and capabilities.
    fn refine(&self, task: Task, now: DateTime<Utc>) -> Task {
        let capabilities = self.capabilities(now);
        let elements: &[Element] = if self.elements_window == self.state.focused { &self.elements } else { &[] };
        activity::refine(task, &self.state, &Signals { elements, capabilities: &capabilities }, now)
    }

    async fn on_task_update(&mut self, update: TrackerUpdate, now: DateTime<Utc>) {
        match &update {
            TrackerUpdate::Unchanged => return,
            TrackerUpdate::Started(t) => info!(task = t.label, confidence = t.confidence, "task started"),
            TrackerUpdate::Changed { from, to } => {
                info!(from = from.label, to = to.label, confidence = to.confidence, "task changed");
                let _ = self.store.record_task_span(
                    &from.label,
                    source_name(from.source),
                    from.confidence,
                    from.since,
                    now,
                );
            }
            TrackerUpdate::Refined(_) => {}
        }
        self.refresh_suggestion(now).await;
        self.adjust_cpu();
        self.publish_surfaces(now).await;
    }

    fn maybe_ask_llm(&mut self, candidate: Option<&Task>, now: DateTime<Utc>) {
        let Some(llm) = self.llm.as_mut() else { return };
        let confidence = candidate.map(|t| t.confidence).unwrap_or(0.0);
        if !llm.should_ask(confidence) {
            return;
        }
        let detail = llm.detail();
        let snapshot = privacy::project(&self.full_snapshot(now, false), detail);
        let llm = self.llm.as_mut().expect("checked above");
        llm.ask(snapshot, self.llm_tx.clone());
    }

    async fn handle_llm(&mut self, answer: LlmAnswer, now: DateTime<Utc>) {
        if let Some(llm) = self.llm.as_mut() {
            llm.finished();
        }
        match answer {
            LlmAnswer::Task { label, confidence } => {
                debug!(label, confidence, "LLM tier answered");
                let members = self.intent.members_for(&self.state, now, &label);
                let task = Task { label, confidence, source: TaskSource::Llm, since: now, members, ..Task::default() };
                let task = self.refine(task, now);
                let update = self.tracker.update(Some(task), now);
                self.on_task_update(update, now).await;
            }
            LlmAnswer::Failed(e) => warn!("LLM tier failed: {e}"),
        }
    }

    async fn refresh_suggestion(&mut self, now: DateTime<Utc>) {
        if self.cfg.suggestions.mode == SuggestMode::Off || !self.state.is_connected() || !self.state.session.observing
        {
            return;
        }
        if self.cooldown_until.is_some_and(|t| now < t) {
            return;
        }
        let Some(task) = self.tracker.current().cloned() else { return };
        let policy = Policy {
            dnd_tasks: &self.cfg.suggestions.dnd_tasks,
            dnd_set_by_ronak: self.dnd_set_by_ronak,
            reserve_right: self.reserved_right(),
        };
        let id = format!("s{}", self.suggestion_seq + 1);
        let Some(next) = suggest::build(id, &self.state, &task, &self.cfg.layout, &policy, now) else {
            self.clear_suggestion().await;
            return;
        };
        if self.suggestion.as_ref().is_some_and(|cur| cur.same_effect(&next)) {
            return;
        }
        self.suggestion_seq += 1;
        match self.cfg.suggestions.mode {
            SuggestMode::Auto => {
                info!(id = next.id, "{}", next.summary);
                self.suggestion = Some(next.clone());
                let _ = self.apply(next, "auto_applied", now).await;
            }
            _ => {
                info!(id = next.id, "suggesting: {}", next.summary);
                let view = next.view();
                let _ = self.store.record_feedback(
                    now,
                    Some(&next.id),
                    "shown",
                    Some(&next.task_label),
                    &json!({ "summary": next.summary }),
                );
                self.suggestion = Some(next);
                adapter::send(&self.commands, AdapterCommand::ShowSuggestion { suggestion: view }).await;
            }
        }
    }

    async fn clear_suggestion(&mut self) {
        if self.suggestion.take().is_some() {
            adapter::send(&self.commands, AdapterCommand::ClearSuggestion).await;
        }
    }

    async fn apply_current(&mut self, id: Option<&str>, now: DateTime<Utc>) -> Result<Value, String> {
        let current = self.suggestion.as_ref().ok_or("there is no suggestion right now")?;
        if id.is_some_and(|id| id != current.id) {
            return Err("that suggestion is out of date".into());
        }
        let s = current.clone();
        self.apply(s, "accepted", now).await
    }

    async fn apply(&mut self, s: Suggestion, kind: &str, now: DateTime<Utc>) -> Result<Value, String> {
        self.undo.record(format!("{} layout", s.task_label), &s.actions, &self.state, now);
        let result = adapter::send(&self.commands, AdapterCommand::Execute { actions: s.actions.clone() }).await;
        for action in &s.actions {
            match action {
                Action::Place { placements } => {
                    for p in placements {
                        self.applied.insert(
                            p.window_id.clone(),
                            Applied { rect: p.rect, at: now, suggestion_id: s.id.clone() },
                        );
                    }
                }
                Action::SetDoNotDisturb { enabled } => self.dnd_set_by_ronak = *enabled,
                _ => {}
            }
        }
        let kind = if result.ok { kind } else { "failed" };
        let _ = self.store.record_feedback(
            now,
            Some(&s.id),
            kind,
            Some(&s.task_label),
            &json!({ "summary": s.summary, "errors": result.errors }),
        );
        self.suggestion = None;
        adapter::send(&self.commands, AdapterCommand::ClearSuggestion).await;
        self.start_cooldown(now);
        if result.ok {
            info!(id = s.id, "applied: {}", s.summary);
            Ok(json!({ "applied": s.id, "summary": s.summary }))
        } else {
            Err(result.errors.join("; "))
        }
    }

    async fn dismiss(&mut self, id: Option<&str>, now: DateTime<Utc>) -> Result<Value, String> {
        let current = self.suggestion.as_ref().ok_or("there is no suggestion right now")?;
        if id.is_some_and(|id| id != current.id) {
            return Err("that suggestion is out of date".into());
        }
        let s = current.clone();
        let _ = self.store.record_feedback(now, Some(&s.id), "dismissed", Some(&s.task_label), &Value::Null);
        self.clear_suggestion().await;
        self.start_cooldown(now);
        Ok(json!({ "dismissed": s.id }))
    }

    async fn undo(&mut self, now: DateTime<Utc>) -> Result<Value, String> {
        let entry = self.undo.pop().ok_or("nothing to undo")?;
        for action in &entry.inverse {
            if let Action::SetDoNotDisturb { enabled } = action {
                self.dnd_set_by_ronak = *enabled && self.dnd_set_by_ronak;
            }
            if let Action::Place { placements } = action {
                for p in placements {
                    self.applied.remove(&p.window_id);
                }
            }
        }
        let result = adapter::send(&self.commands, AdapterCommand::Execute { actions: entry.inverse.clone() }).await;
        let _ = self.store.record_feedback(
            now,
            None,
            "undone",
            self.tracker.current().map(|t| t.label.as_str()),
            &json!({ "label": entry.label }),
        );
        self.start_cooldown(now);
        if result.ok {
            info!("undid {}", entry.label);
            Ok(json!({ "undone": entry.label }))
        } else {
            Err(result.errors.join("; "))
        }
    }

    fn start_cooldown(&mut self, now: DateTime<Utc>) {
        self.cooldown_until = Some(now + Duration::seconds(self.cfg.suggestions.cooldown_secs as i64));
    }

    fn close_task_span(&mut self, now: DateTime<Utc>) {
        if let Some(t) = self.tracker.current() {
            let _ = self.store.record_task_span(&t.label, source_name(t.source), t.confidence, t.since, now);
        }
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        let content_cutoff = now - Duration::hours(self.cfg.storage.content_retention_hours as i64);
        match self.store.digest_content(content_cutoff) {
            Ok(0) => {}
            Ok(n) => info!(
                "condensed {n} pieces of content older than {} hours into pattern digests",
                self.cfg.storage.content_retention_hours
            ),
            Err(e) => warn!("content digest failed: {e}"),
        }
        self.last_digest = Some(now);
        let cutoff = now - Duration::days(self.cfg.storage.raw_retention_days as i64);
        match self.store.prune_raw(cutoff) {
            Ok(0) => {}
            Ok(n) => info!("deleted {n} raw events older than {} days", self.cfg.storage.raw_retention_days),
            Err(e) => warn!("retention cleanup failed: {e}"),
        }
    }

    fn adapter_config(&self) -> ronak_core::protocol::AdapterConfig {
        ronak_core::protocol::AdapterConfig {
            exclusions: self.exclusions.clone(),
            observe_titles: self.cfg.observe.titles,
            activity_interval_ms: self.cfg.observe.activity_interval_ms.max(1000),
            observe_notifications: self.cfg.capabilities.notifications != Access::Off,
        }
    }

    // ----- control socket ------------------------------------------------

    pub async fn handle_control(&mut self, request: ControlRequest, now: DateTime<Utc>) -> Result<Value, String> {
        match request {
            ControlRequest::Status => Ok(self.status(now)),
            ControlRequest::Context { detail } => {
                let snapshot = self.full_snapshot(now, true);
                serde_json::to_value(privacy::project(&snapshot, detail)).map_err(|e| e.to_string())
            }
            ControlRequest::Suggestion { detail } => Ok(self.suggestion_json(detail)),
            ControlRequest::Apply => self.apply_current(None, now).await,
            ControlRequest::Dismiss => self.dismiss(None, now).await,
            ControlRequest::Undo => self.undo(now).await,
            ControlRequest::SetObserving(observing) => {
                self.state.session.observing = observing;
                self.handle_user(UserAction::SetObserving { observing }, now).await
            }
            ControlRequest::Hardware => Ok(self.hardware.clone()),
            ControlRequest::Correct { label, activity, project } => self.correct(label, activity, project, now).await,
            ControlRequest::Subscribe(tx) => {
                self.subscribers.push(tx);
                self.last_published = None; // make the newcomer get the current state
                self.publish_surfaces(now).await;
                Ok(json!({ "subscribed": true }))
            }
            ControlRequest::SurfaceAction { action, text } => self.surface_action(action, text, now).await,
            ControlRequest::Panel { visible, style } => {
                if let Some(style) = style {
                    self.cfg.surfaces.style = style;
                    // Remembered across restarts; the config file sets the default.
                    let _ = self.store.set_meta("panel.style", &serde_json::to_string(&style).unwrap_or_default());
                }
                self.set_panel(visible.or(style.map(|_| self.panel_visible)), now).await
            }
            ControlRequest::ProposeLayout { client, primary, references, note } => {
                self.propose_layout(&client, &primary, &references, note.as_deref(), now).await
            }
            ControlRequest::ProposeSurface { client, surface } => self.propose_surface(&client, &surface, now).await,
            ControlRequest::Patterns { hours } => self.patterns(hours, now),
            ControlRequest::McpCall { .. } => Err("MCP calls run outside the main loop".into()),
        }
    }

    /// Runs an MCP tool call in the background; only with `act` access and
    /// only for tools the server listed.
    fn spawn_mcp_call(&self, server: String, tool: String, args: Value, reply: oneshot::Sender<Result<Value, String>>) {
        let allowed = self.mcp.iter().find(|c| c.name == server).map(|c| (c.access, c.actions.contains(&tool)));
        let refuse = match allowed {
            None => Some(format!("no MCP server named {server:?} in your config")),
            Some((access, _)) if access != Access::Act || self.cfg.capabilities.mcp != Access::Act => {
                Some("MCP servers are read-only in your settings (capabilities.mcp = \"act\" allows calls)".into())
            }
            Some((_, false)) => Some(format!("{server} does not offer a tool named {tool:?}")),
            _ => None,
        };
        if let Some(e) = refuse {
            let _ = reply.send(Err(e));
            return;
        }
        info!(server, tool, "calling MCP tool for you");
        let clients = self.sensors.mcp_clients();
        tokio::spawn(async move {
            let _ = reply.send(crate::sensors::call_mcp_tool(clients, &server, &tool, args).await);
        });
    }

    fn status(&self, now: DateTime<Utc>) -> Value {
        let task = self
            .tracker
            .current()
            .map(|t| json!({ "label": t.label, "confidence": t.confidence, "source": t.source, "since": t.since }));
        let top_apps = self.store.top_apps(now - Duration::hours(24), 5).unwrap_or_default();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_secs": (now - self.started_at).num_seconds(),
            "adapter": self.adapter_name,
            "connected": self.state.is_connected(),
            "platform": self.state.platform,
            "observing": self.state.session.observing,
            "mode": self.cfg.suggestions.mode,
            "windows": self.state.windows.len(),
            "task": task,
            "suggestion": self.suggestion.as_ref().map(|s| s.view()),
            "undo_depth": self.undo.len(),
            "llm_tier": self.llm.as_ref().map(|l| l.describe()),
            "store": self.store.stats().ok(),
            "top_apps_24h": top_apps.into_iter().map(|(app, ms)| json!({ "app": app, "minutes": ms / 60_000 })).collect::<Vec<_>>(),
            "exclusion_rules": self.exclusions.len(),
            "intent": self.tracker.current().map(|t| json!({
                "activity": t.activity, "project": t.project,
                "evidence": t.evidence.iter().filter_map(|e| e.detail.clone()).take(4).collect::<Vec<_>>(),
            })),
            "elements": {
                "enabled": self.sensors.reads_elements(),
                "focused_app_elements": if self.elements_window == self.state.focused { self.elements.len() } else { 0 },
            },
            "capabilities": self.capabilities(now).iter().map(|c| json!({ "id": c.id, "name": c.name, "access": c.access })).collect::<Vec<_>>(),
            "panel": { "visible": self.panel_visible, "style": self.cfg.surfaces.style, "subscribers": self.subscribers.len() },
            "proposals": self.proposals.len(),
            "cpu_lowered": self.lowered_units.len(),
        })
    }

    // ----- schema 0.2: elements, capabilities, surfaces ---------------------

    /// Everything Ronak knows right now, at full detail (content included
    /// when `with_content`), before any privacy projection.
    fn full_snapshot(&self, now: DateTime<Utc>, with_content: bool) -> Snapshot {
        let mut snap = self.state.snapshot(now, self.tracker.current().cloned());
        snap.detail = if with_content { DetailLevel::Content } else { DetailLevel::Full };
        if self.elements_window.is_some() && self.elements_window == self.state.focused {
            snap.elements = self.elements.clone();
            if !with_content {
                for e in &mut snap.elements {
                    e.text = None;
                }
            }
        }
        snap.capabilities = self.capabilities(now);
        snap
    }

    /// All capabilities, with notifications aged out.
    fn capabilities(&self, now: DateTime<Utc>) -> Vec<Capability> {
        let mut caps: Vec<Capability> = self.media.clone();
        caps.extend(self.calendar.clone());
        if self.cfg.capabilities.notifications != Access::Off {
            let recent: Vec<NotificationInfo> =
                self.notifications.iter().filter(|n| now - n.at <= NOTIFICATION_MEMORY).cloned().collect();
            caps.push(Capability {
                id: "notifications".into(),
                kind: CapabilityKind::Notifications,
                provider: self.state.platform.as_ref().map(|p| p.desktop.clone()).unwrap_or_default(),
                name: "Notifications".into(),
                access: self.cfg.capabilities.notifications,
                actions: vec![],
                media: None,
                calendar: None,
                mcp: None,
                notifications: Some(NotificationsState {
                    count_last_hour: recent.len() as u32,
                    recent: recent.into_iter().take(5).collect(),
                }),
            });
        }
        caps.extend(self.mcp.iter().cloned());
        caps
    }

    /// Asks the accessibility sensor to read the focused app: immediately on
    /// focus change, otherwise every `refresh_secs` while you are active.
    fn request_elements(&mut self, now: DateTime<Utc>, focus_changed: bool) {
        if !self.sensors.reads_elements() || !self.state.session.observing || self.element_in_flight {
            return;
        }
        if !focus_changed {
            let due = self
                .element_read_at
                .is_none_or(|t| now - t >= Duration::seconds(self.cfg.elements.refresh_secs as i64));
            if !due || self.state.activity(now).state != ActivityState::Active {
                return;
            }
        }
        let Some(w) = self.state.focused_window() else { return };
        // Never look inside excluded windows (password managers, private browsing).
        if w.redacted {
            self.elements.clear();
            self.elements_window = None;
            return;
        }
        let Some(pid) = w.pid else { return };
        let opts = ReadOptions {
            max_nodes: self.cfg.elements.max_nodes,
            max_depth: self.cfg.elements.max_depth,
            max_text_chars: self.cfg.elements.max_text_chars,
            read_content: self.cfg.elements.read_content,
            ..ReadOptions::default()
        };
        let started = self.sensors.read_elements(
            w.id.clone(),
            pid,
            w.title.clone(),
            w.app_id().map(str::to_owned),
            w.app_name().to_string(),
            opts,
        );
        if started {
            self.element_in_flight = true;
            self.element_read_at = Some(now);
        }
    }

    async fn handle_sensor(&mut self, update: SensorUpdate, now: DateTime<Utc>) {
        match update {
            SensorUpdate::Elements { window_id, app_id, app_name, reading } => {
                self.element_in_flight = false;
                // Ignore readings for a window that lost focus meanwhile.
                if self.state.focused.as_deref() != Some(window_id.as_str()) || !self.state.session.observing {
                    return;
                }
                match reading {
                    Ok(r) if r.exposes => {
                        self.elements = r.elements;
                        self.elements_window = Some(window_id);
                        self.record_content(app_id.as_deref(), now);
                        self.dirty = true;
                    }
                    Ok(_) => {
                        self.elements.clear();
                        self.elements_window = None;
                        self.maybe_ask_accessibility(app_id, app_name, now).await;
                    }
                    Err(e) => debug!("accessibility read failed: {e}"),
                }
            }
            SensorUpdate::Media(players) => {
                if players != self.media {
                    self.media = players;
                    self.dirty = true;
                    self.publish_surfaces(now).await;
                }
            }
            SensorUpdate::Calendar(result) => {
                let cap = result.ok().map(|state| Capability {
                    id: "calendar".into(),
                    kind: CapabilityKind::Calendar,
                    provider: "evolution-data-server".into(),
                    name: "Calendar".into(),
                    access: self.cfg.capabilities.calendar,
                    actions: vec![],
                    media: None,
                    calendar: Some(state),
                    notifications: None,
                    mcp: None,
                });
                if cap != self.calendar {
                    self.calendar = cap;
                    self.dirty = true;
                    self.publish_surfaces(now).await;
                }
            }
            SensorUpdate::Mcp(caps) => {
                self.mcp = caps;
                self.publish_surfaces(now).await;
            }
        }
    }

    /// Keeps text read inside apps for 24 hours (encrypted), skipping repeats.
    fn record_content(&mut self, app_id: Option<&str>, now: DateTime<Utc>) {
        if !self.cfg.elements.read_content {
            return;
        }
        let task = self.tracker.current();
        for e in self.elements.iter().filter(|e| e.text.is_some()) {
            let _ = self.store.record_content(
                now,
                app_id,
                &e.role,
                task.map(|t| t.label.as_str()),
                task.and_then(|t| t.activity.as_deref()),
                task.and_then(|t| t.project.as_deref()),
                e.text.as_deref().unwrap_or_default(),
            );
        }
    }

    async fn maybe_ask_accessibility(&mut self, app_id: Option<String>, app_name: String, now: DateTime<Utc>) {
        let Some(app_id) = app_id else { return };
        if !self.cfg.elements.ask_before_enabling || !ronak_atspi::needs_accessibility_switch(&app_id) {
            return;
        }
        let key = format!("a11y:{app_id}");
        if self.store.get_meta(&key).ok().flatten().is_some()
            || self.a11y_question.as_ref().is_some_and(|(a, _)| *a == app_id)
        {
            return;
        }
        self.a11y_question = Some((app_id, app_name));
        self.publish_surfaces(now).await;
    }

    /// The user corrected the intent: switch now and save it as a label.
    async fn correct(
        &mut self,
        label: String,
        activity: Option<String>,
        project: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Value, String> {
        let label =
            taxonomy::normalize_user_label(&label).ok_or("give a short label, such as coding or grant_writing")?;
        let activity = activity.as_deref().and_then(taxonomy::normalize_user_label);
        let guess = self.tracker.current().cloned();
        let features = json!({
            "focused_app": self.state.focused_window().and_then(|w| w.app_id().map(str::to_owned)),
            "focused_category": self.state.focused_window().map(|w| w.category()),
            "focused_role": self.elements.iter().rev().find(|e| e.has_state("focused")).map(|e| e.role.clone()),
            "open_apps": self.state.windows.values().filter_map(|w| w.app_id().map(str::to_owned)).collect::<Vec<_>>(),
            "switches_last_5min": self.state.activity(now).switches_last_5min,
            "evidence": guess.as_ref().map(|g| g.evidence.iter().map(|e| e.signal.clone()).collect::<Vec<_>>()),
        });
        let g = guess.as_ref().map(|g| ronak_store::Guess {
            label: &g.label,
            activity: g.activity.as_deref(),
            confidence: g.confidence,
        });
        if let Err(e) = self.store.record_label(now, &label, activity.as_deref(), project.as_deref(), g, &features) {
            warn!("could not save label: {e}");
        }
        let members = match &guess {
            Some(g) if g.label == label => g.members.clone(),
            _ => self.intent.members_for(&self.state, now, &label),
        };
        let mut task = Task {
            label: label.clone(),
            confidence: 1.0,
            source: TaskSource::User,
            since: now,
            members,
            activity: activity.clone(),
            project: project.clone(),
            ..Task::default()
        };
        task.evidence.push(ronak_core::model::Evidence {
            signal: "user".into(),
            detail: Some("you said so".into()),
            weight: 1.0,
        });
        let task = self.refine(task, now);
        info!(label, activity = activity.as_deref().unwrap_or(""), "intent corrected by you");
        self.tracker.correct(task, now + Duration::minutes(self.cfg.intent.correction_hold_minutes));
        self.cooldown_until = None;
        self.refresh_suggestion(now).await;
        self.adjust_cpu();
        self.publish_surfaces(now).await;
        Ok(json!({ "label": label, "activity": activity, "saved_as_training_label": true }))
    }

    /// Composes the panel's surfaces and pushes them to subscribers if they changed.
    async fn publish_surfaces(&mut self, now: DateTime<Utc>) {
        if !self.cfg.surfaces.enabled {
            return;
        }
        self.subscribers.retain(|s| !s.is_closed());
        let capabilities = self.capabilities(now);
        let view = self.suggestion.as_ref().map(|s| s.view());
        let question = self.a11y_question.as_ref().map(|(a, n)| (a.as_str(), n.as_str()));
        let input = ComposeInput {
            task: self.tracker.current(),
            capabilities: &capabilities,
            suggestion: view.as_ref(),
            observing: self.state.session.observing,
            undo_available: !self.undo.is_empty(),
            accessibility_question: question,
            now,
        };
        let mut surfaces: Vec<Surface> = RulesComposer.compose(&input);
        surfaces.extend(self.proposals.values().map(|(s, _)| s.clone()));
        // Validate everything, whoever composed it. Invalid surfaces never reach the screen.
        surfaces.retain(|s| match surface::validate(s) {
            Ok(()) => true,
            Err(e) => {
                warn!(surface = s.id, "dropped an invalid surface: {}", e.join("; "));
                false
            }
        });
        // created_at changes on every compose; compare without it.
        let comparable: Vec<Value> = surfaces
            .iter()
            .map(|s| {
                let mut v = serde_json::to_value(s).unwrap_or_default();
                v.as_object_mut().map(|o| o.remove("created_at"));
                v
            })
            .collect();
        let state = json!({ "visible": self.panel_visible, "style": self.cfg.surfaces.style, "width": self.cfg.surfaces.panel_width });
        let fingerprint = json!({ "surfaces": comparable, "panel": state });
        if self.last_published.as_ref() == Some(&fingerprint) {
            return;
        }
        self.last_published = Some(fingerprint);
        let msg = json!({ "jsonrpc": "2.0", "method": "surfaces", "params": { "surfaces": surfaces, "panel": state } });
        self.subscribers.retain(|s| s.try_send(msg.clone()).is_ok() || !s.is_closed());
    }

    async fn surface_action(
        &mut self,
        action: UiAction,
        text: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Value, String> {
        // Surface actions go through the same validation as composed surfaces.
        let mut errors = vec![];
        let probe = surface::Surface {
            schema_version: ronak_core::model::SCHEMA_VERSION.into(),
            id: "action".into(),
            kind: surface::SurfaceKind::Custom,
            title: None,
            composer: surface::ComposerKind::Rules,
            source: None,
            created_at: now,
            root: "b".into(),
            components: vec![surface::Component {
                id: "b".into(),
                body: surface::Body::Button { label: "x".into(), action: action.clone(), icon: None },
            }],
        };
        if let Err(e) = surface::validate(&probe) {
            errors = e;
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        let arg = |k: &str| action.str_arg(k).map(str::to_owned);
        match action.name {
            ActionName::CorrectIntent => {
                let label = text.filter(|t| !t.trim().is_empty()).or_else(|| arg("label")).ok_or("no label given")?;
                self.correct(label, arg("activity"), arg("project"), now).await
            }
            ActionName::AcceptSuggestion => self.apply_current(None, now).await,
            ActionName::DismissSuggestion => self.dismiss(None, now).await,
            ActionName::Undo => self.undo(now).await,
            ActionName::ToggleObserving => {
                let observing = !self.state.session.observing;
                self.state.session.observing = observing;
                self.handle_user(UserAction::SetObserving { observing }, now).await
            }
            ActionName::MediaPlayPause | ActionName::MediaNext | ActionName::MediaPrevious => {
                let id = arg("capability_id").ok_or("which player?")?;
                let cap = self.media.iter().find(|c| c.id == id).ok_or("that player is gone")?;
                if cap.access != Access::Act {
                    return Err(
                        "media control is read-only in your settings (capabilities.media = \"act\" allows it)".into()
                    );
                }
                let command = match action.name {
                    ActionName::MediaNext => MediaCommand::Next,
                    ActionName::MediaPrevious => MediaCommand::Previous,
                    _ => MediaCommand::PlayPause,
                };
                self.sensors.media_control(cap.provider.clone(), command);
                Ok(json!({ "sent": format!("{:?}", action.name) }))
            }
            ActionName::FocusWindow => {
                let id = arg("window_id").ok_or("which window?")?;
                let r = adapter::send(
                    &self.commands,
                    AdapterCommand::Execute { actions: vec![Action::Focus { window_id: id }] },
                )
                .await;
                if r.ok { Ok(json!({ "focused": true })) } else { Err(r.errors.join("; ")) }
            }
            ActionName::AcceptProposal | ActionName::DismissProposal => {
                let id = arg("proposal_id").ok_or("which proposal?")?;
                let accept = action.name == ActionName::AcceptProposal;
                let entry = self.proposals.get_mut(&id).ok_or("that proposal is gone")?;
                let agent = entry.0.source.clone();
                if accept {
                    entry.1 = true;
                    // A kept card loses its Keep button; Dismiss stays so you can remove it later.
                    let comps = &mut entry.0.components;
                    comps.retain(|c| c.id != "proposal-accept");
                    for c in comps.iter_mut() {
                        if let surface::Body::Row { children } = &mut c.body {
                            children.retain(|ch| ch != "proposal-accept");
                        }
                        if c.id == "proposal-dismiss"
                            && let surface::Body::Button { label, .. } = &mut c.body
                        {
                            *label = "Remove".into();
                        }
                    }
                } else {
                    self.proposals.remove(&id);
                }
                let kind = if accept { "proposal_kept" } else { "proposal_dismissed" };
                let _ = self.store.record_feedback(now, Some(&id), kind, None, &json!({ "agent": agent }));
                self.publish_surfaces(now).await;
                Ok(json!({ kind: id }))
            }
            ActionName::ClosePanel => self.set_panel(Some(false), now).await,
            ActionName::EnableAccessibility | ActionName::SkipAccessibility => {
                let app = arg("app_id").ok_or("which app?")?;
                let enable = action.name == ActionName::EnableAccessibility;
                let _ = self.store.set_meta(&format!("a11y:{app}"), if enable { "enabled" } else { "skipped" });
                self.a11y_question = None;
                let result = if enable {
                    crate::sensors::enable_accessibility()
                        .await
                        .map(|_| json!({ "enabled": true, "note": "restart the app so it shares its contents" }))
                } else {
                    Ok(json!({ "skipped": app }))
                };
                self.publish_surfaces(now).await;
                result
            }
        }
    }

    fn reserved_right(&self) -> i32 {
        if self.panel_visible && self.cfg.surfaces.enabled && self.cfg.surfaces.style == surface::PanelStyle::SidePanel
        {
            self.cfg.surfaces.panel_width
        } else {
            0
        }
    }

    async fn set_panel(&mut self, visible: Option<bool>, now: DateTime<Utc>) -> Result<Value, String> {
        self.panel_visible = visible.unwrap_or(!self.panel_visible);
        self.arrange_panel().await;
        self.publish_surfaces(now).await;
        // The space available for layouts changed.
        self.dirty = true;
        Ok(json!({ "visible": self.panel_visible, "style": self.cfg.surfaces.style }))
    }

    async fn arrange_panel(&mut self) {
        if !self.cfg.surfaces.enabled {
            return;
        }
        let cmd = AdapterCommand::ArrangePanel {
            visible: self.panel_visible,
            style: self.cfg.surfaces.style,
            width: self.cfg.surfaces.panel_width,
        };
        let r = adapter::send(&self.commands, cmd).await;
        if !r.ok {
            debug!("panel arrangement: {}", r.errors.join("; "));
        }
    }

    /// Resolves "Code", "code" or a window id to a visible window.
    fn find_window(&self, needle: &str) -> Option<String> {
        let n = needle.trim().to_lowercase();
        let candidates = ronak_core::intent::layout_candidates(&self.state);
        candidates
            .iter()
            .find(|w| w.id == needle)
            .or_else(|| candidates.iter().find(|w| w.app_id().is_some_and(|a| a.to_lowercase() == n)))
            .or_else(|| candidates.iter().find(|w| w.app_name().to_lowercase() == n))
            .or_else(|| {
                candidates.iter().find(|w| {
                    w.app_name().to_lowercase().contains(&n)
                        || w.app_id().is_some_and(|a| a.to_lowercase().contains(&n))
                })
            })
            .map(|w| w.id.clone())
    }

    /// An agent proposes a layout. It becomes a normal suggestion that waits
    /// for your key press.
    async fn propose_layout(
        &mut self,
        client: &str,
        primary: &str,
        references: &[String],
        note: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Value, String> {
        let primary_id = self.find_window(primary).ok_or_else(|| format!("no visible window matches {primary:?}"))?;
        let mut members = vec![TaskMember { window_id: primary_id.clone(), role: Role::Primary, weight: 1.0 }];
        for r in references.iter().take(3) {
            let id = self.find_window(r).ok_or_else(|| format!("no visible window matches {r:?}"))?;
            if !members.iter().any(|m| m.window_id == id) {
                members.push(TaskMember { window_id: id, role: Role::Reference, weight: 0.6 });
            }
        }
        let label = self.tracker.current().map(|t| t.label.clone()).unwrap_or_else(|| "proposal".into());
        let task = Task { label, confidence: 1.0, source: TaskSource::User, since: now, members, ..Task::default() };
        let policy = Policy { dnd_tasks: &[], dnd_set_by_ronak: false, reserve_right: self.reserved_right() };
        self.suggestion_seq += 1;
        let id = format!("a{}", self.suggestion_seq);
        let mut s = suggest::build(id, &self.state, &task, &self.cfg.layout, &policy, now)
            .ok_or("those windows are already arranged like that")?;
        s.summary = match note {
            Some(n) => format!("{client} suggests: {} ({})", s.summary, n.chars().take(80).collect::<String>()),
            None => format!("{client} suggests: {}", s.summary),
        };
        let view = s.view();
        let _ = self.store.record_feedback(
            now,
            Some(&s.id),
            "proposed",
            Some(&s.task_label),
            &json!({ "agent": client, "summary": s.summary }),
        );
        self.suggestion = Some(s);
        adapter::send(&self.commands, AdapterCommand::ShowSuggestion { suggestion: view.clone() }).await;
        self.publish_surfaces(now).await;
        Ok(json!({ "suggestion_id": view.id, "summary": view.summary, "status": "waiting for the user to accept" }))
    }

    /// An agent proposes a surface; it is validated, wrapped with Keep and
    /// Dismiss buttons and shown in the panel.
    async fn propose_surface(&mut self, client: &str, value: &Value, now: DateTime<Utc>) -> Result<Value, String> {
        let mut value = value.clone();
        if let Some(o) = value.as_object_mut() {
            o.entry("schema_version").or_insert(json!(ronak_core::model::SCHEMA_VERSION));
            o.entry("id").or_insert(json!("proposal"));
            o.entry("kind").or_insert(json!("custom"));
            o.insert("composer".into(), json!("agent"));
            o.insert("created_at".into(), json!(now));
        }
        let parsed = surface::parse(&value).map_err(|e| format!("the surface is not valid: {}", e.join("; ")))?;
        self.proposal_seq += 1;
        let id = format!("p{}", self.proposal_seq);
        let wrapped = surface::wrap_proposal(parsed, &id, client);
        surface::validate(&wrapped).map_err(|e| e.join("; "))?;
        // Keep at most three pending proposals.
        while self.proposals.iter().filter(|(_, (_, kept))| !kept).count() >= 3 {
            let oldest = self.proposals.iter().find(|(_, (_, kept))| !kept).map(|(k, _)| k.clone()).expect("counted");
            self.proposals.remove(&oldest);
        }
        self.proposals.insert(id.clone(), (wrapped, false));
        let _ = self.store.record_feedback(now, Some(&id), "proposed", None, &json!({ "agent": client }));
        self.publish_surfaces(now).await;
        Ok(json!({ "proposal_id": id, "status": "shown in the Ronak panel; waiting for the user" }))
    }

    /// Gives apps outside the current task a lower CPU weight, and restores
    /// apps that rejoin it. Never touches the focused app, anything playing
    /// media or in a meeting.
    fn adjust_cpu(&mut self) {
        if !self.cfg.resources.lower_background_priority || !self.state.session.observing {
            return;
        }
        let Some(task) = self.tracker.current() else { return };
        let playing = self
            .media
            .iter()
            .any(|c| c.media.as_ref().is_some_and(|m| m.status == ronak_core::model::PlaybackStatus::Playing));
        let mut unit_roles: HashMap<String, bool> = HashMap::new(); // unit -> may be lowered
        for w in self.state.windows.values() {
            let Some(pid) = w.pid else { continue };
            let Some(unit) = ronak_caps::cpu::app_unit_of_pid(pid) else { continue };
            let background = task.member(&w.id).is_none_or(|m| m.role == Role::Background) || w.minimized;
            let protected = self.state.focused.as_deref() == Some(w.id.as_str())
                || matches!(w.category(), ronak_core::model::AppCategory::Meeting)
                || (playing
                    && matches!(
                        w.category(),
                        ronak_core::model::AppCategory::Media | ronak_core::model::AppCategory::Browser
                    ));
            let lowerable = background && !protected;
            let entry = unit_roles.entry(unit).or_insert(true);
            *entry &= lowerable;
        }
        for (unit, lower) in unit_roles {
            let is_lowered = self.lowered_units.contains(&unit);
            if lower && !is_lowered {
                self.sensors.set_cpu_weight(unit.clone(), self.cfg.resources.background_cpu_weight);
                self.lowered_units.insert(unit);
            } else if !lower && is_lowered {
                self.sensors.set_cpu_weight(unit.clone(), ronak_caps::cpu::NORMAL_WEIGHT);
                self.lowered_units.remove(&unit);
            }
        }
    }

    fn restore_cpu(&mut self) {
        for unit in self.lowered_units.drain() {
            self.sensors.set_cpu_weight(unit, ronak_caps::cpu::NORMAL_WEIGHT);
        }
    }

    /// Patterns condensed from content older than a day, plus focus totals.
    fn patterns(&self, hours: i64, now: DateTime<Utc>) -> Result<Value, String> {
        let since = now - Duration::hours(hours.clamp(1, 24 * 90));
        let digests = self.store.digests(since).map_err(|e| e.to_string())?;
        let mut tasks: BTreeMap<String, u64> = BTreeMap::new();
        let mut projects: BTreeMap<String, u64> = BTreeMap::new();
        let mut words: BTreeMap<String, u64> = BTreeMap::new();
        for (_, d) in &digests {
            for (k, v) in &d.tasks {
                *tasks.entry(k.clone()).or_default() += v;
            }
            for (k, v) in &d.projects {
                *projects.entry(k.clone()).or_default() += v;
            }
            for (k, v) in &d.keywords {
                *words.entry(k.clone()).or_default() += v;
            }
        }
        let mut words: Vec<(String, u64)> = words.into_iter().collect();
        words.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        words.truncate(20);
        let top_apps = self.store.top_apps(since, 10).unwrap_or_default();
        Ok(json!({
            "since": since,
            "hours_digested": digests.len(),
            "tasks": tasks,
            "projects": projects,
            "frequent_words": words,
            "top_apps": top_apps.into_iter().map(|(a, ms)| json!({ "app": a, "minutes": ms / 60_000 })).collect::<Vec<_>>(),
        }))
    }

    fn suggestion_json(&self, detail: DetailLevel) -> Value {
        let Some(s) = &self.suggestion else { return json!({ "suggestion": null }) };
        if !detail.allows_windows() {
            return json!({ "suggestion": { "id": s.id, "task_label": s.task_label } });
        }
        let placements: Vec<Value> = s
            .plan
            .placements
            .iter()
            .map(|p| {
                let w = self.state.windows.get(&p.window_id);
                let role = self.tracker.current().and_then(|t| t.member(&p.window_id)).map(|m| m.role);
                json!({
                    "window_id": p.window_id,
                    "app": w.map(|w| w.app_name()),
                    "title": w.and_then(|w| if detail.allows_titles() { w.title.clone() } else { None }),
                    "role": role,
                    "rect": p.rect,
                })
            })
            .collect();
        json!({ "suggestion": { "id": s.id, "task_label": s.task_label, "summary": s.summary, "placements": placements, "actions": s.actions } })
    }

    /// For tests and the mock: the current suggestion.
    #[cfg(test)]
    pub fn current_suggestion(&self) -> Option<&Suggestion> {
        self.suggestion.as_ref()
    }
}

fn source_name(source: TaskSource) -> &'static str {
    match source {
        TaskSource::Heuristic => "heuristic",
        TaskSource::Classifier => "classifier",
        TaskSource::Llm => "llm",
        TaskSource::User => "user",
    }
}

/// Exclusion rules that would hide everything are almost certainly a mistake.
pub fn sanity_check(cfg: &Config) -> Vec<String> {
    let mut warnings = vec![];
    for rule in cfg.observe.effective_exclusions() {
        if rule.mode == ExclusionMode::Ignore && rule.app_id.as_deref() == Some("*") && rule.title_contains.is_none() {
            warnings.push("an exclusion rule ignores every app; Ronak will see nothing".into());
        }
    }
    if cfg.intent.llm.enabled && cfg.intent.llm.allow_remote {
        warnings.push("the LLM tier may send context to a non-local server (intent.llm.allow_remote = true)".into());
    }
    warnings
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ronak_core::model::*;
    use ronak_core::protocol::*;

    use super::*;

    const AREA: Rect = Rect::new(0, 32, 1920, 1168);

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T10:00:00Z").unwrap().with_timezone(&Utc) + Duration::seconds(secs)
    }

    fn win(id: &str, app: &str, title: &str) -> Window {
        Window {
            id: id.into(),
            app: Some(App { id: app.into(), name: app.into(), category: AppCategory::Other }),
            title: Some(title.into()),
            kind: WindowKind::Normal,
            rect: AREA,
            monitor: 0,
            workspace: Some(0),
            focused: false,
            minimized: false,
            maximized: true,
            fullscreen: false,
            redacted: false,
            last_focused_at: None,
            pid: None,
        }
    }

    /// A daemon wired to a fake adapter that records every command.
    fn harness() -> (Daemon, Arc<Mutex<Vec<AdapterCommand>>>) {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<AdapterRequest>(64);
        let log = Arc::new(Mutex::new(vec![]));
        let sink = log.clone();
        tokio::spawn(async move {
            while let Some(req) = cmd_rx.recv().await {
                sink.lock().unwrap().push(req.command);
                let _ = req.reply.send(ronak_core::protocol::CommandResult::ok());
            }
        });
        let mut cfg = Config::default();
        cfg.suggestions.cooldown_secs = 0;
        let (llm_tx, _llm_rx) = mpsc::channel(1);
        let (sensor_tx, _sensor_rx) = mpsc::channel(1);
        let d = Daemon::new(
            cfg,
            Store::open_in_memory().unwrap(),
            cmd_tx,
            "test",
            json!({}),
            None,
            llm_tx,
            Sensors::disabled(sensor_tx),
        );
        (d, log)
    }

    /// Hello plus a one-monitor desktop with the given windows.
    async fn desk(d: &mut Daemon, windows: Vec<Window>) {
        d.handle_event(
            PlatformEvent::Hello { platform: PlatformInfo::unknown(), protocol_version: PROTOCOL_VERSION },
            t(0),
        )
        .await;
        d.handle_event(
            PlatformEvent::Snapshot {
                monitors: vec![Monitor {
                    index: 0,
                    geometry: Rect::new(0, 0, 1920, 1200),
                    work_area: AREA,
                    scale: 1.0,
                    primary: true,
                }],
                workspaces: None,
                windows,
                focused_window: None,
                idle_ms: 0,
                session: Session::default(),
            },
            t(0),
        )
        .await;
    }

    /// Subscribes like the panel does and returns the receiver.
    async fn subscribe(d: &mut Daemon, at: i64) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel(16);
        d.handle_control(ControlRequest::Subscribe(tx), t(at)).await.unwrap();
        rx
    }

    fn latest(rx: &mut mpsc::Receiver<Value>) -> Value {
        let mut last = Value::Null;
        while let Ok(v) = rx.try_recv() {
            last = v;
        }
        last
    }

    fn surface<'a>(msg: &'a Value, kind: &str) -> Option<&'a Value> {
        msg["params"]["surfaces"].as_array()?.iter().find(|s| s["kind"] == kind)
    }

    fn texts(surface: &Value) -> Vec<String> {
        surface["components"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.get("text").or_else(|| c.get("label")).and_then(Value::as_str).map(str::to_owned))
            .collect()
    }

    fn executed(log: &Arc<Mutex<Vec<AdapterCommand>>>) -> Vec<Vec<Action>> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                AdapterCommand::Execute { actions } => Some(actions.clone()),
                _ => None,
            })
            .collect()
    }

    async fn focus(d: &mut Daemon, id: &str, at: i64) {
        d.handle_event(PlatformEvent::FocusChanged { window_id: Some(id.into()) }, t(at)).await;
        d.on_tick(t(at)).await;
    }

    #[tokio::test]
    async fn suggest_accept_undo_meeting_and_back() {
        let (mut d, log) = harness();
        d.handle_event(
            PlatformEvent::Hello {
                platform: PlatformInfo {
                    os: Os::Linux,
                    desktop: "gnome".into(),
                    desktop_version: "46".into(),
                    adapter: "test".into(),
                    adapter_version: "0".into(),
                },
                protocol_version: PROTOCOL_VERSION,
            },
            t(0),
        )
        .await;
        d.handle_event(
            PlatformEvent::Snapshot {
                monitors: vec![Monitor {
                    index: 0,
                    geometry: Rect::new(0, 0, 1920, 1200),
                    work_area: AREA,
                    scale: 1.0,
                    primary: true,
                }],
                workspaces: Some(Workspaces { active: 0, count: 1 }),
                windows: vec![
                    win("1", "code", "main.rs"),
                    win("2", "Alacritty", "cargo build"),
                    win("3", "firefox", "tokio - docs.rs"),
                    win("9", "org.keepassxc.KeePassXC", "bank.kdbx"),
                ],
                focused_window: None,
                idle_ms: 0,
                session: Session::default(),
            },
            t(0),
        )
        .await;
        assert!(matches!(log.lock().unwrap()[0], AdapterCommand::Configure { .. }), "config is pushed on hello");

        // Privacy: the password manager's title never reaches state or storage.
        let full = d.handle_control(ControlRequest::Context { detail: DetailLevel::Full }, t(0)).await.unwrap();
        let kp = full["windows"].as_array().unwrap().iter().find(|w| w["id"] == "9").unwrap();
        assert!(kp.get("title").is_none() && kp["redacted"] == true);

        // A coding session leads to a suggestion.
        focus(&mut d, "1", 0).await;
        focus(&mut d, "2", 40).await;
        focus(&mut d, "3", 60).await;
        focus(&mut d, "1", 80).await;
        d.on_tick(t(100)).await;
        let s = d.current_suggestion().cloned().expect("a coding suggestion");
        assert_eq!(s.task_label, "coding");
        assert!(s.summary.starts_with("coding: code main"), "{}", s.summary);

        // Accepting it moves windows, and undo restores them maximized.
        d.handle_event(
            PlatformEvent::User { action: UserAction::AcceptSuggestion { suggestion_id: s.id.clone() } },
            t(101),
        )
        .await;
        let placed = executed(&log);
        let Action::Place { placements } = &placed[0][0] else { panic!("expected a placement") };
        assert_eq!(placements[0].window_id, "1");
        assert!(
            !placements.iter().any(|p| p.window_id == "9"),
            "an unused password manager is not part of the coding layout"
        );
        d.handle_event(PlatformEvent::User { action: UserAction::Undo }, t(102)).await;
        let undone = executed(&log);
        let Action::Place { placements } = &undone[1][0] else { panic!("expected an undo placement") };
        assert!(placements.iter().all(|p| p.maximized && p.rect == AREA));

        // A meeting starts: notifications should pause.
        d.handle_event(PlatformEvent::WindowOpened { window: win("5", "us.zoom.Zoom", "Zoom Meeting") }, t(110)).await;
        focus(&mut d, "5", 110).await;
        d.on_tick(t(200)).await;
        d.on_tick(t(215)).await;
        let s = d.current_suggestion().cloned().expect("a meeting suggestion");
        assert_eq!(s.task_label, "meeting");
        assert!(s.actions.contains(&Action::SetDoNotDisturb { enabled: true }));
        d.handle_event(PlatformEvent::User { action: UserAction::AcceptSuggestion { suggestion_id: s.id } }, t(203))
            .await;
        d.handle_event(
            PlatformEvent::SessionChanged { session: Session { do_not_disturb: true, observing: true } },
            t(203),
        )
        .await;

        // The meeting ends: back to coding, and Ronak turns notifications back on.
        d.handle_event(PlatformEvent::WindowClosed { window_id: "5".into() }, t(300)).await;
        focus(&mut d, "1", 300).await;
        for s in [400, 415, 430] {
            d.on_tick(t(s)).await;
        }
        let s = d.current_suggestion().cloned().expect("back to coding");
        assert_eq!(s.task_label, "coding");
        assert!(s.actions.contains(&Action::SetDoNotDisturb { enabled: false }), "{:?}", s.actions);

        let status = d.handle_control(ControlRequest::Status, t(431)).await.unwrap();
        assert_eq!(status["task"]["label"], "coding");
        assert!(status["store"]["feedback"].as_i64().unwrap() >= 4);
    }

    #[tokio::test]
    async fn manual_moves_after_apply_are_recorded_as_overrides() {
        let (mut d, _log) = harness();
        d.handle_event(
            PlatformEvent::Hello { platform: PlatformInfo::unknown(), protocol_version: PROTOCOL_VERSION },
            t(0),
        )
        .await;
        d.handle_event(
            PlatformEvent::Snapshot {
                monitors: vec![Monitor {
                    index: 0,
                    geometry: Rect::new(0, 0, 1920, 1200),
                    work_area: AREA,
                    scale: 1.0,
                    primary: true,
                }],
                workspaces: None,
                windows: vec![win("1", "code", "a"), win("2", "Alacritty", "b")],
                focused_window: Some("1".into()),
                idle_ms: 0,
                session: Session::default(),
            },
            t(0),
        )
        .await;
        focus(&mut d, "2", 30).await;
        focus(&mut d, "1", 45).await;
        d.on_tick(t(60)).await;
        let before = d.store.stats().unwrap().feedback;
        let r = d.handle_control(ControlRequest::Apply, t(61)).await.unwrap();
        assert!(r["applied"].is_string());
        let mut moved = win("2", "Alacritty", "b");
        moved.maximized = false;
        moved.rect = Rect::new(100, 100, 700, 500);
        d.handle_event(PlatformEvent::WindowChanged { window: moved }, t(70)).await;
        assert_eq!(d.store.stats().unwrap().feedback, before + 2, "accepted + overridden");
    }

    #[tokio::test]
    async fn corrections_switch_now_hold_and_are_saved() {
        let (mut d, _log) = harness();
        desk(
            &mut d,
            vec![win("1", "code", "main.rs - ronak - Visual Studio Code"), win("2", "Alacritty", "~/src/ronak")],
        )
        .await;
        let mut panel = subscribe(&mut d, 0).await;
        focus(&mut d, "1", 0).await;
        focus(&mut d, "2", 30).await;
        focus(&mut d, "1", 50).await;
        d.on_tick(t(70)).await;
        assert_eq!(d.tracker.current().unwrap().label, "coding");
        let msg = latest(&mut panel);
        let intent = surface(&msg, "intent_panel").expect("an intent panel");
        assert!(texts(intent).iter().any(|t| t.contains("Coding")), "{:?}", texts(intent));

        // "No, I'm writing the grant proposal."
        let r = d
            .handle_control(
                ControlRequest::Correct {
                    label: "Grant writing".into(),
                    activity: None,
                    project: Some("grant".into()),
                },
                t(80),
            )
            .await
            .unwrap();
        assert_eq!(r["label"], "grant_writing");
        let task = d.tracker.current().unwrap();
        assert_eq!((task.label.as_str(), task.source), ("grant_writing", TaskSource::User));
        assert!(!task.members.is_empty(), "the corrected task keeps the windows in use");
        assert_eq!(d.store.stats().unwrap().labels, 1, "saved as a training label");

        // The heuristic still sees coding, but the correction holds.
        for s in [100, 115, 130, 300] {
            d.on_tick(t(s)).await;
        }
        assert_eq!(d.tracker.current().unwrap().label, "grant_writing");
        let msg = latest(&mut panel);
        let intent = surface(&msg, "intent_panel").unwrap();
        assert!(texts(intent).iter().any(|t| t == "you said so"), "{:?}", texts(intent));

        // The panel's entry sends the same correction.
        let action: UiAction = serde_json::from_value(json!({ "name": "correct_intent", "args": {} })).unwrap();
        d.handle_control(ControlRequest::SurfaceAction { action, text: Some("reading".into()) }, t(310)).await.unwrap();
        assert_eq!(d.tracker.current().unwrap().label, "reading");
    }

    #[tokio::test]
    async fn notifications_respect_detail_levels() {
        let (mut d, _log) = harness();
        desk(&mut d, vec![win("1", "code", "a")]).await;
        let n = NotificationInfo {
            app: "Slack".into(),
            title: Some("Rojin".into()),
            body: Some("can you review my PR?".into()),
            at: t(5),
            urgency: Urgency::Normal,
        };
        d.handle_event(PlatformEvent::Notification { notification: n }, t(5)).await;
        let get = |ctx: &Value| -> Value {
            ctx["capabilities"].as_array().unwrap().iter().find(|c| c["kind"] == "notifications").cloned().unwrap()
        };
        let content = d.handle_control(ControlRequest::Context { detail: DetailLevel::Content }, t(6)).await.unwrap();
        let c = get(&content);
        assert_eq!(c["notifications"]["count_last_hour"], 1);
        assert_eq!(c["notifications"]["recent"][0]["body"], "can you review my PR?");
        let full = d.handle_control(ControlRequest::Context { detail: DetailLevel::Full }, t(6)).await.unwrap();
        let f = get(&full);
        assert_eq!(f["notifications"]["recent"][0]["title"], "Rojin");
        assert!(f["notifications"]["recent"][0].get("body").is_none(), "body only at content level");
        let apps = d.handle_control(ControlRequest::Context { detail: DetailLevel::TaskAndApps }, t(6)).await.unwrap();
        let a = get(&apps);
        assert_eq!(a["notifications"]["recent"][0]["app"], "Slack");
        assert!(a["notifications"]["recent"][0].get("title").is_none());
        // An hour later it is forgotten.
        let later = d.handle_control(ControlRequest::Context { detail: DetailLevel::Content }, t(3700)).await.unwrap();
        assert_eq!(get(&later)["notifications"]["count_last_hour"], 0);
    }

    #[tokio::test]
    async fn agent_proposals_are_validated_and_wait_for_the_user() {
        let (mut d, _log) = harness();
        desk(&mut d, vec![win("1", "code", "a"), win("2", "firefox", "b")]).await;
        let mut panel = subscribe(&mut d, 0).await;

        // A broken surface never reaches the panel.
        let bad = json!({ "root": "x", "components": [ { "id": "x", "type": "button", "label": "Run", "action": { "name": "rm_rf" } } ] });
        assert!(
            d.handle_control(ControlRequest::ProposeSurface { client: "agent".into(), surface: bad }, t(1))
                .await
                .is_err()
        );

        let good = json!({
            "title": "Review queue",
            "root": "c",
            "components": [
                { "id": "c", "type": "card", "title": "2 PRs wait for you", "children": ["t"] },
                { "id": "t", "type": "text", "text": "ronak#12 and ronak#14" }
            ]
        });
        let r = d
            .handle_control(ControlRequest::ProposeSurface { client: "claude-code".into(), surface: good }, t(2))
            .await
            .unwrap();
        let id = r["proposal_id"].as_str().unwrap().to_string();
        let msg = latest(&mut panel);
        let p = surface(&msg, "proposal").expect("the proposal is shown");
        assert_eq!(p["composer"], "agent");
        assert!(texts(p).iter().any(|t| t == "Keep"));

        let dismiss: UiAction =
            serde_json::from_value(json!({ "name": "dismiss_proposal", "args": { "proposal_id": id } })).unwrap();
        let keep: UiAction =
            serde_json::from_value(json!({ "name": "accept_proposal", "args": { "proposal_id": id } })).unwrap();
        d.handle_control(ControlRequest::SurfaceAction { action: keep, text: None }, t(3)).await.unwrap();
        let msg = latest(&mut panel);
        let kept = texts(surface(&msg, "proposal").expect("still shown"));
        assert!(!kept.contains(&"Keep".to_string()) && kept.contains(&"Remove".to_string()), "{kept:?}");
        d.handle_control(ControlRequest::SurfaceAction { action: dismiss, text: None }, t(3)).await.unwrap();
        assert!(surface(&latest(&mut panel), "proposal").is_none());

        // A layout proposal becomes a normal suggestion the user still has to accept.
        let r = d
            .handle_control(
                ControlRequest::ProposeLayout {
                    client: "claude-code".into(),
                    primary: "firefox".into(),
                    references: vec!["Code".into()],
                    note: Some("you are reading the docs".into()),
                },
                t(4),
            )
            .await
            .unwrap();
        assert_eq!(r["status"], "waiting for the user to accept");
        let s = d.current_suggestion().unwrap();
        assert!(s.summary.starts_with("claude-code suggests:"), "{}", s.summary);
        assert!(
            d.handle_control(
                ControlRequest::ProposeLayout {
                    client: "x".into(),
                    primary: "nope".into(),
                    references: vec![],
                    note: None
                },
                t(5)
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn the_side_panel_reserves_space_for_layouts() {
        let (mut d, log) = harness();
        desk(&mut d, vec![win("1", "code", "a"), win("2", "Alacritty", "b")]).await;
        d.handle_control(ControlRequest::Panel { visible: Some(true), style: None }, t(1)).await.unwrap();
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, AdapterCommand::ArrangePanel { visible: true, width: 360, .. }))
        );
        focus(&mut d, "1", 2).await;
        focus(&mut d, "2", 30).await;
        focus(&mut d, "1", 50).await;
        d.on_tick(t(70)).await;
        let s = d.current_suggestion().cloned().expect("a suggestion");
        let right = s.plan.placements.iter().map(|p| p.rect.x + p.rect.w).max().unwrap();
        assert!(right <= 1920 - 360, "layout leaves room for the panel (right edge {right})");
        // Switching to the floating card frees the space again and is remembered.
        let r = d
            .handle_control(
                ControlRequest::Panel { visible: None, style: Some(surface::PanelStyle::FloatingCard) },
                t(71),
            )
            .await
            .unwrap();
        assert_eq!((r["visible"].as_bool(), r["style"].as_str()), (Some(true), Some("floating_card")));
        assert_eq!(d.reserved_right(), 0);
        assert_eq!(d.store.get_meta("panel.style").unwrap().as_deref(), Some("\"floating_card\""));
        let r = d.handle_control(ControlRequest::Panel { visible: None, style: None }, t(72)).await.unwrap();
        assert_eq!(r["visible"], false);
    }
}

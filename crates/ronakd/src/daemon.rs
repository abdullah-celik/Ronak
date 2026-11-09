//! The daemon's main loop: one task that owns all state.
//!
//! Everything that can happen arrives as a message on a channel: desktop
//! events from the adapter, requests from the control socket, answers from
//! the LLM tier, and timer ticks. Because a single task handles them one at a
//! time, no locks are needed anywhere in the daemon (the "actor" pattern).

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use ronak_core::actions::UndoStack;
use ronak_core::adapter::{self, AdapterRequest};
use ronak_core::config::{Config, SuggestMode};
use ronak_core::intent::{HeuristicIntent, IntentModel, TaskTracker, TrackerUpdate};
use ronak_core::model::{ActivityState, DetailLevel, Rect, Task, TaskSource};
use ronak_core::privacy::{self, ExclusionMode, ExclusionRule};
use ronak_core::protocol::{Action, AdapterCommand, PlatformEvent, UserAction};
use ronak_core::state::{Change, DesktopState};
use ronak_core::suggest::{self, Policy, Suggestion};
use ronak_store::Store;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::llm::{LlmAnswer, LlmTier};

/// A request from the control socket (CLI and MCP bridge).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    Status,
    Context { detail: DetailLevel },
    Suggestion { detail: DetailLevel },
    Apply,
    Dismiss,
    Undo,
    SetObserving(bool),
    Hardware,
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
}

impl Daemon {
    pub fn new(
        cfg: Config,
        store: Store,
        commands: mpsc::Sender<AdapterRequest>,
        adapter_name: &'static str,
        hardware: Value,
        llm: Option<LlmTier>,
        llm_tx: mpsc::Sender<LlmAnswer>,
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
        }
    }

    /// Runs until `shutdown` completes or every input closes.
    pub async fn run(
        mut self,
        mut events: mpsc::Receiver<PlatformEvent>,
        mut control: mpsc::Receiver<ControlMsg>,
        mut llm_rx: mpsc::Receiver<LlmAnswer>,
        shutdown: impl std::future::Future<Output = ()>,
    ) {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
        let mut prune = tokio::time::interval(std::time::Duration::from_secs(3600));
        tokio::pin!(shutdown);
        info!(adapter = self.adapter_name, "ronakd is running");
        loop {
            tokio::select! {
                Some(event) = events.recv() => self.handle_event(event, Utc::now()).await,
                Some(msg) = control.recv() => {
                    let result = self.handle_control(msg.request, Utc::now()).await;
                    let _ = msg.reply.send(result);
                }
                Some(answer) = llm_rx.recv() => self.handle_llm(answer, Utc::now()).await,
                _ = tick.tick() => self.on_tick(Utc::now()).await,
                _ = prune.tick() => self.prune(Utc::now()),
                _ = &mut shutdown => break,
            }
        }
        self.close_task_span(Utc::now());
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
                }
                Ok(json!({ "observing": observing }))
            }
        }
    }

    // ----- intent and suggestions ----------------------------------------

    pub async fn on_tick(&mut self, now: DateTime<Utc>) {
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
        let candidate = self.intent.infer(&self.state, now);
        self.maybe_ask_llm(candidate.as_ref(), now);
        let update = self.tracker.update(candidate, now);
        self.on_task_update(update, now).await;
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
    }

    fn maybe_ask_llm(&mut self, candidate: Option<&Task>, now: DateTime<Utc>) {
        let Some(llm) = self.llm.as_mut() else { return };
        let confidence = candidate.map(|t| t.confidence).unwrap_or(0.0);
        if !llm.should_ask(confidence) {
            return;
        }
        let snapshot = privacy::project(&self.state.snapshot(now, None), llm.detail());
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
                let task = Task { label, confidence, source: TaskSource::Llm, since: now, members };
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
        let policy = Policy { dnd_tasks: &self.cfg.suggestions.dnd_tasks, dnd_set_by_ronak: self.dnd_set_by_ronak };
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
        }
    }

    // ----- control socket ------------------------------------------------

    pub async fn handle_control(&mut self, request: ControlRequest, now: DateTime<Utc>) -> Result<Value, String> {
        match request {
            ControlRequest::Status => Ok(self.status(now)),
            ControlRequest::Context { detail } => {
                let snapshot = self.state.snapshot(now, self.tracker.current().cloned());
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
        }
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
        })
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
        let d = Daemon::new(cfg, Store::open_in_memory().unwrap(), cmd_tx, "test", json!({}), None, llm_tx);
        (d, log)
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
}

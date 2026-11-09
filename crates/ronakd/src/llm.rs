//! Tier 2: a local LLM, consulted only when the heuristic is unsure.
//!
//! Talks to any OpenAI-compatible server (Ollama, Lemonade, llama.cpp
//! server, OpenVINO Model Server) with one short request. The server decides
//! where the model runs: NPU, GPU or CPU. Ronak only chooses *which* server,
//! and by default refuses anything that is not on this machine.

use std::time::{Duration, Instant};

use ronak_core::config::LlmConfig;
use ronak_core::model::{DetailLevel, KNOWN_TASKS, Snapshot};
use ronak_hw::LocalServer;
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub enum LlmAnswer {
    Task { label: String, confidence: f32 },
    Failed(String),
}

pub struct LlmTier {
    client: reqwest::Client,
    cfg: LlmConfig,
    last_call: Option<Instant>,
    in_flight: bool,
}

/// True for localhost and loopback addresses.
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    match parsed.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

impl LlmTier {
    pub fn new(cfg: LlmConfig) -> Result<Self, String> {
        if !cfg.allow_remote && !is_loopback_url(&cfg.endpoint) {
            return Err(format!(
                "LLM endpoint {} is not on this machine; set intent.llm.allow_remote = true if you really mean it",
                cfg.endpoint
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, cfg, last_call: None, in_flight: false })
    }

    pub fn detail(&self) -> DetailLevel {
        self.cfg.detail
    }

    pub fn describe(&self) -> Value {
        json!({ "endpoint": self.cfg.endpoint, "model": self.cfg.model, "detail": self.cfg.detail, "below_confidence": self.cfg.below_confidence })
    }

    pub fn should_ask(&self, heuristic_confidence: f32) -> bool {
        !self.in_flight
            && heuristic_confidence < self.cfg.below_confidence
            && self.last_call.is_none_or(|t| t.elapsed() >= Duration::from_secs(self.cfg.min_interval_secs))
    }

    pub fn finished(&mut self) {
        self.in_flight = false;
    }

    /// Starts one classification in the background; the answer arrives on `tx`.
    pub fn ask(&mut self, snapshot: Snapshot, tx: mpsc::Sender<LlmAnswer>) {
        self.in_flight = true;
        self.last_call = Some(Instant::now());
        let client = self.client.clone();
        let cfg = self.cfg.clone();
        tokio::spawn(async move {
            let answer = match classify(&client, &cfg, &snapshot).await {
                Ok((label, confidence)) => LlmAnswer::Task { label, confidence },
                Err(e) => LlmAnswer::Failed(e),
            };
            let _ = tx.send(answer).await;
        });
    }
}

const SYSTEM_PROMPT: &str = "You infer what a person is doing on their computer from the windows they have open. \
Answer with JSON only: {\"task\": \"<label>\", \"confidence\": <0..1>}. \
Prefer one of these labels: coding, writing, reading, research, communication, meeting, design, media, admin. \
Use a different short lowercase label only if none fits.";

/// Builds a compact, model-friendly description of the snapshot.
pub fn describe_snapshot(s: &Snapshot) -> String {
    let mut lines = vec![];
    for w in &s.windows {
        let app = w
            .app
            .as_ref()
            .map(|a| format!("{} ({:?})", a.name, a.category).to_lowercase())
            .unwrap_or_else(|| "unknown app".into());
        let mut line = format!("- {app}");
        if w.focused {
            line.push_str(", focused");
        }
        if let Some(t) = w.last_focused_at {
            let ago = (s.captured_at - t).num_seconds().max(0);
            line.push_str(&format!(", last focused {ago}s ago"));
        }
        if let Some(title) = &w.title {
            line.push_str(&format!(", title: {title}"));
        }
        lines.push(line);
    }
    format!(
        "Open windows:\n{}\nSwitches between windows in the last 5 minutes: {}.",
        if lines.is_empty() { "(none visible)".to_string() } else { lines.join("\n") },
        s.activity.switches_last_5min
    )
}

async fn classify(client: &reqwest::Client, cfg: &LlmConfig, snapshot: &Snapshot) -> Result<(String, f32), String> {
    let body = json!({
        "model": cfg.model,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": describe_snapshot(snapshot) }
        ]
    });
    let url = format!("{}/chat/completions", cfg.endpoint.trim_end_matches('/'));
    let resp: Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let content = resp["choices"][0]["message"]["content"].as_str().ok_or("response has no message content")?;
    parse_answer(content)
}

/// Accepts the model's JSON, tolerating code fences, and validates the label.
pub fn parse_answer(content: &str) -> Result<(String, f32), String> {
    let trimmed = content.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    let v: Value = serde_json::from_str(trimmed).map_err(|e| format!("model did not return JSON: {e}"))?;
    let label = v["task"].as_str().ok_or("missing \"task\"")?.trim().to_lowercase();
    let valid =
        !label.is_empty() && label.len() <= 32 && label.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '-');
    if !valid {
        return Err(format!("invalid task label {label:?}"));
    }
    let confidence = v["confidence"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0) as f32;
    // Unknown labels are allowed but trusted less.
    let confidence = if KNOWN_TASKS.contains(&label.as_str()) { confidence } else { confidence * 0.8 };
    Ok((label, confidence))
}

/// Looks for local inference servers on their default ports.
pub async fn detect_servers() -> Vec<LocalServer> {
    let client = match reqwest::Client::builder().timeout(Duration::from_millis(800)).build() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let probes = [
        ("ollama", "http://127.0.0.1:11434/api/tags", "http://127.0.0.1:11434/v1"),
        ("lemonade", "http://127.0.0.1:8000/api/v1/models", "http://127.0.0.1:8000/api/v1"),
        ("llama.cpp", "http://127.0.0.1:8080/v1/models", "http://127.0.0.1:8080/v1"),
    ];
    let mut found = vec![];
    for (kind, probe, base) in probes {
        let Ok(resp) = client.get(probe).send().await else { continue };
        let Ok(v) = resp.json::<Value>().await else { continue };
        let models: Vec<String> = v["models"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(str::to_owned)).collect())
            .or_else(|| {
                v["data"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_owned)).collect())
            })
            .unwrap_or_default();
        found.push(LocalServer { kind: kind.into(), base_url: base.into(), models });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_by_default() {
        assert!(is_loopback_url("http://127.0.0.1:11434/v1"));
        assert!(is_loopback_url("http://localhost:8000/api/v1"));
        assert!(is_loopback_url("http://[::1]:8080/v1"));
        assert!(!is_loopback_url("https://api.example.com/v1"));
        assert!(!is_loopback_url("http://192.168.1.20:11434/v1"));
        let cfg = LlmConfig { enabled: true, endpoint: "https://api.example.com/v1".into(), ..LlmConfig::default() };
        assert!(LlmTier::new(cfg).is_err());
    }

    #[test]
    fn parses_model_answers() {
        assert_eq!(parse_answer("{\"task\": \"coding\", \"confidence\": 0.9}").unwrap(), ("coding".into(), 0.9));
        assert_eq!(parse_answer("```json\n{\"task\":\"Meeting\"}\n```").unwrap().0, "meeting");
        let (label, c) = parse_answer("{\"task\":\"gardening\",\"confidence\":1.0}").unwrap();
        assert_eq!((label.as_str(), c), ("gardening", 0.8));
        assert!(parse_answer("{\"task\":\"rm -rf /\"}").is_err());
        assert!(parse_answer("I think coding").is_err());
    }
}

//! User configuration. Every field has a safe default, so an empty or missing
//! config file is valid. The daemon reads it from
//! `~/.config/ronak/config.toml`; see `config/ronak.example.toml`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::layout::LayoutConfig;
use crate::model::DetailLevel;
use crate::privacy::{ExclusionRule, default_exclusions};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub observe: ObserveConfig,
    pub storage: StorageConfig,
    pub intent: IntentConfig,
    pub layout: LayoutConfig,
    pub suggestions: SuggestionConfig,
    pub mcp: McpConfig,
    pub gnome: GnomeConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObserveConfig {
    /// Record window titles at all (still subject to exclusions).
    pub titles: bool,
    /// Start from the built-in exclusions (password managers, private browsing).
    pub use_default_exclusions: bool,
    /// Your own rules, added to the defaults.
    pub exclusions: Vec<ExclusionRule>,
    pub activity_interval_ms: u64,
}

impl Default for ObserveConfig {
    fn default() -> Self {
        Self { titles: true, use_default_exclusions: true, exclusions: vec![], activity_interval_ms: 15_000 }
    }
}

impl ObserveConfig {
    pub fn effective_exclusions(&self) -> Vec<ExclusionRule> {
        let mut rules = if self.use_default_exclusions { default_exclusions() } else { vec![] };
        rules.extend(self.exclusions.iter().cloned());
        rules
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encryption {
    /// SQLCipher with a random key kept in the OS keyring
    /// (Secret Service on Linux, Credential Manager on Windows, Keychain on macOS).
    Keyring,
    /// Plain SQLite. Only for development and tests.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub encryption: Encryption,
    /// Raw events older than this are deleted; hourly aggregates are kept.
    pub raw_retention_days: u32,
    /// Overrides the data directory (default: ~/.local/share/ronak).
    pub data_dir: Option<String>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { encryption: Encryption::Keyring, raw_retention_days: 30, data_dir: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IntentConfig {
    /// Below this, a new task guess is ignored.
    pub min_confidence: f32,
    pub lookback_minutes: i64,
    pub half_life_secs: f64,
    pub min_evidence_secs: f64,
    pub llm: LlmConfig,
}

impl Default for IntentConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.4,
            lookback_minutes: 10,
            half_life_secs: 180.0,
            min_evidence_secs: 20.0,
            llm: LlmConfig::default(),
        }
    }
}

/// Tier 2: a local LLM behind any OpenAI-compatible server (Ollama, Lemonade,
/// llama.cpp server, OpenVINO Model Server). Consulted only when the
/// heuristic is unsure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    /// Base URL ending in /v1.
    pub endpoint: String,
    pub model: String,
    /// Refuse non-loopback endpoints unless this is true.
    pub allow_remote: bool,
    /// What the model may see.
    pub detail: DetailLevel,
    /// Ask the LLM only when the heuristic's confidence is below this.
    pub below_confidence: f32,
    pub min_interval_secs: u64,
    pub timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:3b".into(),
            allow_remote: false,
            detail: DetailLevel::TaskAndApps,
            below_confidence: 0.5,
            min_interval_secs: 60,
            timeout_secs: 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestMode {
    /// Never suggest or act.
    Off,
    /// Show a suggestion; the user accepts it with one key.
    Suggest,
    /// Apply suggestions automatically (still undoable).
    Auto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SuggestionConfig {
    pub mode: SuggestMode,
    /// After a suggestion is accepted or dismissed, wait this long before the next.
    pub cooldown_secs: u64,
    /// Tasks during which notifications are silenced.
    pub dnd_tasks: Vec<String>,
}

impl Default for SuggestionConfig {
    fn default() -> Self {
        Self { mode: SuggestMode::Suggest, cooldown_secs: 30, dnd_tasks: vec!["meeting".into()] }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    /// What AI agents see unless listed below.
    pub default_detail: DetailLevel,
    /// Per-client overrides, keyed by the name the client reports, e.g.
    /// `"claude-code" = "full"`.
    pub clients: BTreeMap<String, DetailLevel>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self { default_detail: DetailLevel::TaskAndApps, clients: BTreeMap::new() }
    }
}

impl McpConfig {
    pub fn detail_for(&self, client: &str) -> DetailLevel {
        self.clients.get(client).copied().unwrap_or(self.default_detail)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GnomeConfig {
    /// Accept adapter messages only from processes running these executables.
    pub trusted_shell_paths: Vec<String>,
    /// Turn off only for debugging.
    pub verify_peers: bool,
}

impl Default for GnomeConfig {
    fn default() -> Self {
        Self { trusted_shell_paths: vec!["/usr/bin/gnome-shell".into()], verify_peers: true }
    }
}

//! # ronak-store
//!
//! Everything Ronak remembers lives in one SQLite database, encrypted with
//! SQLCipher. The 256-bit key is random, generated on first run and kept in
//! the operating system's keyring (Secret Service / GNOME Keyring on Linux,
//! Credential Manager on Windows, Keychain on macOS), so the file on disk is
//! unreadable without your login session.
//!
//! Tables:
//! - `events`: raw observations. Deleted after the retention period (30 days).
//! - `app_usage_hourly`: focus time per app per hour. Kept: small and useful.
//! - `task_spans`: what task the engine believed in, and when.
//! - `feedback`: accepted, dismissed, undone and overridden suggestions; the
//!   training signal for learning your preferences later.
//! - `content` (v2): text read inside apps. Kept 24 hours, then condensed
//!   into `digests` (counts, projects, frequent words per hour) and deleted.
//! - `labels` (v2): your corrections of the intent, with what Ronak had
//!   guessed. Training data for the on-device model.

use std::path::Path;

use chrono::{DateTime, Duration, DurationRound, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the database could not be opened with the stored key (wrong key, or not a Ronak database)")]
    WrongKey,
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("could not generate a random key: {0}")]
    Random(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

const SCHEMA_VERSION: i64 = 2;

const MIGRATION_1: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    id        INTEGER PRIMARY KEY,
    ts_ms     INTEGER NOT NULL,
    kind      TEXT NOT NULL,
    window_id TEXT,
    app_id    TEXT,
    title     TEXT,
    data      TEXT
);
CREATE INDEX IF NOT EXISTS events_ts ON events (ts_ms);
CREATE TABLE IF NOT EXISTS app_usage_hourly (
    hour_start_ms INTEGER NOT NULL,
    app_id        TEXT NOT NULL,
    focus_ms      INTEGER NOT NULL,
    focus_count   INTEGER NOT NULL,
    PRIMARY KEY (hour_start_ms, app_id)
);
CREATE TABLE IF NOT EXISTS task_spans (
    id         INTEGER PRIMARY KEY,
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER NOT NULL,
    label      TEXT NOT NULL,
    source     TEXT NOT NULL,
    confidence REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS feedback (
    id            INTEGER PRIMARY KEY,
    ts_ms         INTEGER NOT NULL,
    suggestion_id TEXT,
    kind          TEXT NOT NULL,
    task_label    TEXT,
    data          TEXT
);
";

const MIGRATION_2: &str = "
CREATE TABLE IF NOT EXISTS content (
    id         INTEGER PRIMARY KEY,
    ts_ms      INTEGER NOT NULL,
    app_id     TEXT,
    role       TEXT NOT NULL,
    task_label TEXT,
    activity   TEXT,
    project    TEXT,
    text       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS content_ts ON content (ts_ms);
CREATE TABLE IF NOT EXISTS digests (
    hour_start_ms INTEGER PRIMARY KEY,
    data          TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS labels (
    id                 INTEGER PRIMARY KEY,
    ts_ms              INTEGER NOT NULL,
    label              TEXT NOT NULL,
    activity           TEXT,
    project            TEXT,
    guessed_label      TEXT,
    guessed_activity   TEXT,
    guessed_confidence REAL,
    features           TEXT
);
";

const MIGRATIONS: &[&str] = &[MIGRATION_1, MIGRATION_2];

/// What Ronak had guessed when you corrected it.
#[derive(Debug, Clone, PartialEq)]
pub struct Guess<'a> {
    pub label: &'a str,
    pub activity: Option<&'a str>,
    pub confidence: f32,
}

/// One hour of condensed content.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, Serialize)]
pub struct Digest {
    pub items: u64,
    pub apps: std::collections::BTreeMap<String, u64>,
    pub roles: std::collections::BTreeMap<String, u64>,
    pub tasks: std::collections::BTreeMap<String, u64>,
    pub activities: std::collections::BTreeMap<String, u64>,
    pub projects: std::collections::BTreeMap<String, u64>,
    /// Words that appeared at least twice, most frequent first.
    pub keywords: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoreStats {
    pub events: i64,
    pub oldest_event: Option<DateTime<Utc>>,
    pub task_spans: i64,
    pub feedback: i64,
    pub content: i64,
    pub digests: i64,
    pub labels: i64,
    pub encrypted: bool,
}

pub struct Store {
    conn: Connection,
    encrypted: bool,
}

impl Store {
    /// Opens (or creates) the database at `path`. With a key, the file is
    /// encrypted with SQLCipher; without one it is plain SQLite.
    pub fn open(path: &Path, key: Option<&[u8; 32]>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn, key)
    }

    /// An in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, key: Option<&[u8; 32]>) -> Result<Self> {
        if let Some(key) = key {
            // "x'..'" tells SQLCipher this is a raw 256-bit key, so it skips
            // the slow passphrase derivation. The hex comes from our own bytes.
            conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex(key)))?;
        }
        // The first real read fails if the key is wrong.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::NotADatabase => StoreError::WrongKey,
            other => StoreError::Sqlite(other),
        })?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Store { conn, encrypted: key.is_some() };
        store.migrate()?;
        Ok(store)
    }

    /// Applies every migration newer than the database, in one transaction each.
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        let current: i64 = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get::<_, String>(0))
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let version = i as i64 + 1;
            if version <= current {
                continue;
            }
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![version.to_string()],
            )?;
            tx.commit()?;
        }
        debug_assert_eq!(MIGRATIONS.len() as i64, SCHEMA_VERSION);
        Ok(())
    }

    /// Stores text read inside an app, unless the same text from the same app
    /// and role was stored in the last hour.
    #[allow(clippy::too_many_arguments)]
    pub fn record_content(
        &self,
        ts: DateTime<Utc>,
        app_id: Option<&str>,
        role: &str,
        task_label: Option<&str>,
        activity: Option<&str>,
        project: Option<&str>,
        text: &str,
    ) -> Result<bool> {
        let last: Option<String> = self
            .conn
            .query_row(
                "SELECT text FROM content WHERE app_id IS ?1 AND role = ?2 AND ts_ms >= ?3 ORDER BY ts_ms DESC LIMIT 1",
                params![app_id, role, (ts - Duration::hours(1)).timestamp_millis()],
                |r| r.get(0),
            )
            .optional()?;
        if last.as_deref() == Some(text) {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO content (ts_ms, app_id, role, task_label, activity, project, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![ts.timestamp_millis(), app_id, role, task_label, activity, project, text],
        )?;
        Ok(true)
    }

    /// Condenses content older than `before` into hourly digests, then
    /// deletes it. Returns how many content rows were digested.
    pub fn digest_content(&self, before: DateTime<Utc>) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        struct Row {
            ts: i64,
            app: Option<String>,
            role: String,
            task: Option<String>,
            activity: Option<String>,
            project: Option<String>,
            text: String,
        }
        let rows: Vec<Row> = {
            let mut stmt = tx.prepare(
                "SELECT ts_ms, app_id, role, task_label, activity, project, text FROM content WHERE ts_ms < ?1 ORDER BY ts_ms",
            )?;
            stmt.query_map(params![before.timestamp_millis()], |r| {
                Ok(Row {
                    ts: r.get(0)?,
                    app: r.get(1)?,
                    role: r.get(2)?,
                    task: r.get(3)?,
                    activity: r.get(4)?,
                    project: r.get(5)?,
                    text: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        let mut hours: std::collections::BTreeMap<i64, (Digest, std::collections::HashMap<String, u64>)> =
            Default::default();
        for Row { ts, app, role, task, activity, project, text } in &rows {
            let hour = ts - ts.rem_euclid(3_600_000);
            let (d, words) = hours.entry(hour).or_default();
            d.items += 1;
            let bump = |m: &mut std::collections::BTreeMap<String, u64>, k: &Option<String>| {
                if let Some(k) = k {
                    *m.entry(k.clone()).or_default() += 1;
                }
            };
            bump(&mut d.apps, app);
            *d.roles.entry(role.clone()).or_default() += 1;
            bump(&mut d.tasks, task);
            bump(&mut d.activities, activity);
            bump(&mut d.projects, project);
            for w in keywords(text) {
                *words.entry(w).or_default() += 1;
            }
        }
        for (hour, (mut d, words)) in hours {
            // Merge with an existing digest for the same hour.
            let existing: Option<String> = tx
                .query_row("SELECT data FROM digests WHERE hour_start_ms = ?1", params![hour], |r| r.get(0))
                .optional()?;
            let mut counts: std::collections::HashMap<String, u64> = words;
            if let Some(old) = existing.and_then(|j| serde_json::from_str::<Digest>(&j).ok()) {
                d.items += old.items;
                for (map, other) in [
                    (&mut d.apps, old.apps),
                    (&mut d.roles, old.roles),
                    (&mut d.tasks, old.tasks),
                    (&mut d.activities, old.activities),
                    (&mut d.projects, old.projects),
                ] {
                    for (k, v) in other {
                        *map.entry(k).or_default() += v;
                    }
                }
                for (k, v) in old.keywords {
                    *counts.entry(k).or_default() += v;
                }
            }
            let mut kw: Vec<(String, u64)> = counts.into_iter().filter(|(_, n)| *n >= 2).collect();
            kw.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            kw.truncate(15);
            d.keywords = kw;
            tx.execute(
                "INSERT INTO digests (hour_start_ms, data) VALUES (?1, ?2)
                 ON CONFLICT (hour_start_ms) DO UPDATE SET data = excluded.data",
                params![hour, serde_json::to_string(&d).unwrap_or_default()],
            )?;
        }
        tx.execute("DELETE FROM content WHERE ts_ms < ?1", params![before.timestamp_millis()])?;
        tx.commit()?;
        Ok(rows.len())
    }

    /// A small persistent setting, such as a per-app decision.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Hourly digests since `since`, oldest first.
    pub fn digests(&self, since: DateTime<Utc>) -> Result<Vec<(DateTime<Utc>, Digest)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT hour_start_ms, data FROM digests WHERE hour_start_ms >= ?1 ORDER BY hour_start_ms")?;
        let rows =
            stmt.query_map(params![since.timestamp_millis()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = vec![];
        for row in rows {
            let (ms, json) = row?;
            if let (Some(t), Ok(d)) = (Utc.timestamp_millis_opt(ms).single(), serde_json::from_str(&json)) {
                out.push((t, d));
            }
        }
        Ok(out)
    }

    /// Saves a correction you made, with what Ronak had guessed and the
    /// (non-content) features at that moment.
    pub fn record_label(
        &self,
        ts: DateTime<Utc>,
        label: &str,
        activity: Option<&str>,
        project: Option<&str>,
        guess: Option<Guess>,
        features: &Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO labels (ts_ms, label, activity, project, guessed_label, guessed_activity, guessed_confidence, features)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                ts.timestamp_millis(),
                label,
                activity,
                project,
                guess.as_ref().map(|g| g.label),
                guess.as_ref().and_then(|g| g.activity),
                guess.as_ref().map(|g| g.confidence as f64),
                features.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn record_event(
        &self,
        ts: DateTime<Utc>,
        kind: &str,
        window_id: Option<&str>,
        app_id: Option<&str>,
        title: Option<&str>,
        data: &Value,
    ) -> Result<()> {
        let data = if data.is_null() { None } else { Some(data.to_string()) };
        self.conn.execute(
            "INSERT INTO events (ts_ms, kind, window_id, app_id, title, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ts.timestamp_millis(), kind, window_id, app_id, title, data],
        )?;
        Ok(())
    }

    /// Adds focus time for an app, split across the hours it spans.
    pub fn add_focus_time(&self, app_id: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<()> {
        let mut t = start;
        while t < end {
            let hour = t.duration_trunc(Duration::hours(1)).unwrap_or(t);
            let segment_end = end.min(hour + Duration::hours(1));
            let ms = (segment_end - t).num_milliseconds();
            if ms > 0 {
                self.conn.execute(
                    "INSERT INTO app_usage_hourly (hour_start_ms, app_id, focus_ms, focus_count) VALUES (?1, ?2, ?3, 1)
                     ON CONFLICT (hour_start_ms, app_id) DO UPDATE SET
                        focus_ms = focus_ms + excluded.focus_ms,
                        focus_count = focus_count + 1",
                    params![hour.timestamp_millis(), app_id, ms],
                )?;
            }
            t = segment_end;
        }
        Ok(())
    }

    pub fn record_task_span(
        &self,
        label: &str,
        source: &str,
        confidence: f32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_spans (start_ms, end_ms, label, source, confidence) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![start.timestamp_millis(), end.timestamp_millis(), label, source, confidence as f64],
        )?;
        Ok(())
    }

    /// `kind` is one of: shown, accepted, dismissed, undone, overridden, auto_applied.
    pub fn record_feedback(
        &self,
        ts: DateTime<Utc>,
        suggestion_id: Option<&str>,
        kind: &str,
        task_label: Option<&str>,
        data: &Value,
    ) -> Result<()> {
        let data = if data.is_null() { None } else { Some(data.to_string()) };
        self.conn.execute(
            "INSERT INTO feedback (ts_ms, suggestion_id, kind, task_label, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ts.timestamp_millis(), suggestion_id, kind, task_label, data],
        )?;
        Ok(())
    }

    /// Deletes raw events older than `before`. Aggregates are kept.
    pub fn prune_raw(&self, before: DateTime<Utc>) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM events WHERE ts_ms < ?1", params![before.timestamp_millis()])?)
    }

    /// Total focus time per app since `since`, most used first.
    pub fn top_apps(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT app_id, SUM(focus_ms) AS ms FROM app_usage_hourly
             WHERE hour_start_ms >= ?1 GROUP BY app_id ORDER BY ms DESC, app_id LIMIT ?2",
        )?;
        let since_hour = since.duration_trunc(Duration::hours(1)).unwrap_or(since);
        let rows =
            stmt.query_map(params![since_hour.timestamp_millis(), limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn stats(&self) -> Result<StoreStats> {
        let count = |table: &str| -> Result<i64> {
            Ok(self.conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
        };
        let oldest: Option<i64> = self.conn.query_row("SELECT MIN(ts_ms) FROM events", [], |r| r.get(0))?;
        Ok(StoreStats {
            events: count("events")?,
            oldest_event: oldest.and_then(|ms| Utc.timestamp_millis_opt(ms).single()),
            task_spans: count("task_spans")?,
            feedback: count("feedback")?,
            content: count("content")?,
            digests: count("digests")?,
            labels: count("labels")?,
            encrypted: self.encrypted,
        })
    }
}

/// Loads the database key from the OS keyring, creating one on first use.
pub fn keyring_key(service: &str, account: &str) -> Result<[u8; 32]> {
    let entry = keyring::Entry::new(service, account).map_err(|e| StoreError::Keyring(e.to_string()))?;
    match entry.get_password() {
        Ok(stored) => unhex(&stored).ok_or_else(|| StoreError::Keyring("stored key is malformed".into())),
        Err(keyring::Error::NoEntry) => {
            let mut key = [0u8; 32];
            getrandom::fill(&mut key).map_err(|e| StoreError::Random(e.to_string()))?;
            entry.set_password(&hex(&key)).map_err(|e| StoreError::Keyring(e.to_string()))?;
            Ok(key)
        }
        Err(e) => Err(StoreError::Keyring(e.to_string())),
    }
}

/// Words worth remembering from a piece of text: lowercase, 4 to 24 letters,
/// no digits, not common filler words.
fn keywords(text: &str) -> impl Iterator<Item = String> + '_ {
    const STOP: &[&str] = &[
        "this", "that", "with", "from", "have", "your", "will", "they", "what", "when", "there", "their", "about",
        "would", "which", "into", "than", "then", "them", "were", "been", "some", "more", "also", "just", "like",
        "only", "over", "here", "these", "those", "does", "should", "could", "each", "very", "make", "made", "much",
        "many", "most",
    ];
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| (4..=24).contains(&w.chars().count()))
        .map(str::to_lowercase)
        .filter(|w| !STOP.contains(&w.as_str()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn content_is_digested_then_deleted() {
        let s = Store::open_in_memory().unwrap();
        let rec = |ts: &str, text: &str| {
            s.record_content(t(ts), Some("code"), "terminal", Some("coding"), Some("testing"), Some("ronak"), text)
                .unwrap()
        };
        assert!(rec("2026-11-08T10:05:00Z", "running tests for the daemon module"));
        assert!(!rec("2026-11-08T10:06:00Z", "running tests for the daemon module"), "duplicates are skipped");
        assert!(rec("2026-11-08T10:07:00Z", "daemon tests failed: mismatched types in daemon"));
        assert!(rec("2026-11-09T09:00:00Z", "fresh text stays"));
        let digested = s.digest_content(t("2026-11-08T12:00:00Z")).unwrap();
        assert_eq!(digested, 2);
        assert_eq!(s.stats().unwrap().content, 1, "only the fresh row remains");
        let digests = s.digests(t("2026-11-08T00:00:00Z")).unwrap();
        assert_eq!(digests.len(), 1);
        let (hour, d) = &digests[0];
        assert_eq!(*hour, t("2026-11-08T10:00:00Z"));
        assert_eq!((d.items, d.projects["ronak"], d.activities["testing"]), (2, 2, 2));
        assert_eq!(d.keywords[0], ("daemon".to_string(), 3));
        assert!(!d.keywords.iter().any(|(w, _)| w == "mismatched"), "words seen once are not kept");
        // Digesting again merges into the same hour.
        s.record_content(t("2026-11-08T10:30:00Z"), Some("code"), "terminal", None, None, None, "daemon again")
            .unwrap();
        s.digest_content(t("2026-11-08T12:00:00Z")).unwrap();
        assert_eq!(s.digests(t("2026-11-08T00:00:00Z")).unwrap()[0].1.items, 3);
    }

    #[test]
    fn labels_and_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ronak.db");
        {
            // A version-1 database, as written by Ronak 0.1.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_1).unwrap();
            conn.execute("INSERT INTO meta (key, value) VALUES ('schema_version', '1')", []).unwrap();
            conn.execute("INSERT INTO events (ts_ms, kind) VALUES (1, 'focus')", []).unwrap();
        }
        let s = Store::open(&path, None).unwrap();
        assert_eq!(s.stats().unwrap().events, 1, "old data survives the upgrade");
        s.record_label(
            t("2026-11-09T10:00:00Z"),
            "writing",
            Some("drafting"),
            None,
            Some(Guess { label: "coding", activity: Some("writing_code"), confidence: 0.6 }),
            &serde_json::json!({ "focused_app": "code" }),
        )
        .unwrap();
        assert_eq!(s.stats().unwrap().labels, 1);
        let version: String =
            s.conn.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).unwrap();
        assert_eq!(version, "2");
    }

    #[test]
    fn hex_round_trip() {
        let key = [7u8; 32];
        assert_eq!(unhex(&hex(&key)), Some(key));
        assert_eq!(unhex("zz"), None);
    }

    #[test]
    fn encrypted_file_needs_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ronak.db");
        let key = [42u8; 32];
        {
            let s = Store::open(&path, Some(&key)).unwrap();
            s.record_event(t("2026-11-09T10:00:00Z"), "focus", Some("1"), Some("code"), Some("secret.rs"), &json!({}))
                .unwrap();
        }
        // The title must not appear in plain text on disk.
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(9).any(|w| w == b"secret.rs"));
        assert!(matches!(Store::open(&path, Some(&[1u8; 32])), Err(StoreError::WrongKey)));
        assert!(matches!(Store::open(&path, None), Err(StoreError::WrongKey)));
        let s = Store::open(&path, Some(&key)).unwrap();
        assert_eq!(s.stats().unwrap().events, 1);
        assert!(s.stats().unwrap().encrypted);
    }

    #[test]
    fn focus_time_splits_across_hours_and_prunes() {
        let s = Store::open_in_memory().unwrap();
        s.add_focus_time("code", t("2026-11-09T10:50:00Z"), t("2026-11-09T11:10:00Z")).unwrap();
        s.add_focus_time("code", t("2026-11-09T11:20:00Z"), t("2026-11-09T11:25:00Z")).unwrap();
        s.add_focus_time("firefox", t("2026-11-09T11:00:00Z"), t("2026-11-09T11:01:00Z")).unwrap();
        let top = s.top_apps(t("2026-11-09T00:00:00Z"), 10).unwrap();
        assert_eq!(top, vec![("code".to_string(), 25 * 60_000), ("firefox".to_string(), 60_000)]);
        let since_11 = s.top_apps(t("2026-11-09T11:30:00Z"), 10).unwrap();
        assert_eq!(since_11[0], ("code".to_string(), 15 * 60_000), "hour buckets are kept whole");

        s.record_event(t("2026-10-01T00:00:00Z"), "focus", None, None, None, &Value::Null).unwrap();
        assert_eq!(s.stats().unwrap().labels, 0);
        s.record_event(t("2026-11-09T00:00:00Z"), "focus", None, None, None, &Value::Null).unwrap();
        assert_eq!(s.prune_raw(t("2026-10-10T00:00:00Z")).unwrap(), 1);
        assert_eq!(s.stats().unwrap().events, 1);
    }
}

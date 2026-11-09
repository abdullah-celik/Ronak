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

const SCHEMA_VERSION: i64 = 1;

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

#[derive(Debug, Clone, Serialize)]
pub struct StoreStats {
    pub events: i64,
    pub oldest_event: Option<DateTime<Utc>>,
    pub task_spans: i64,
    pub feedback: i64,
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

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(MIGRATION_1)?;
        let version: Option<String> =
            self.conn.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).optional()?;
        if version.is_none() {
            self.conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )?;
        }
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
        s.record_event(t("2026-11-09T00:00:00Z"), "focus", None, None, None, &Value::Null).unwrap();
        assert_eq!(s.prune_raw(t("2026-10-10T00:00:00Z")).unwrap(), 1);
        assert_eq!(s.stats().unwrap().events, 1);
    }
}

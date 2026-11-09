//! GNOME's calendar store, Evolution Data Server (EDS), over D-Bus.
//!
//! Three steps, verified against EDS 3.52 (Ubuntu 24.04):
//! 1. The source registry (`org.gnome.evolution.dataserver.Sources5`) lists
//!    every configured source; calendar sources have a `[Calendar]` section
//!    in their key-file `Data` property.
//! 2. The calendar factory (`...Calendar8`) `OpenCalendar(uid)` returns the
//!    object path and bus name of a backend. The backend lives as long as
//!    the connection that opened it, so the reader keeps one connection.
//! 3. `GetObjectList(query)` returns iCalendar text for events in a time
//!    range; [`crate::ical`] expands recurrences and time zones.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use ronak_core::model::{CalendarEvent, CalendarState};
use tracing::debug;
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::ical;
use crate::{CapsError, Result};

const SOURCES: &str = "org.gnome.evolution.dataserver.Sources5";
const SOURCE_MANAGER: &str = "/org/gnome/evolution/dataserver/SourceManager";
const FACTORY_BUS: &str = "org.gnome.evolution.dataserver.Calendar8";
const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/CalendarFactory";
const CALENDAR_IFACE: &str = "org.gnome.evolution.dataserver.Calendar";

type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// Is this source key-file an enabled calendar? Birthday calendars built
/// from contacts are skipped: they are all-day noise for intent.
pub fn is_enabled_calendar(data: &str) -> bool {
    let mut section = "";
    let mut has_calendar = false;
    let mut enabled = true;
    for line in data.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            section = line;
            has_calendar |= section == "[Calendar]";
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match (section, k.trim(), v.trim()) {
                ("[Data Source]" | "[Calendar]", "Enabled", "false") => enabled = false,
                ("[Calendar]", "BackendName", "contacts") => enabled = false,
                _ => {}
            }
        }
    }
    has_calendar && enabled
}

pub struct CalendarReader {
    conn: Connection,
    /// Source uid -> (bus name, object path) of opened backends.
    opened: HashMap<String, (String, String)>,
}

impl CalendarReader {
    pub fn new(conn: Connection) -> Self {
        Self { conn, opened: HashMap::new() }
    }

    async fn calendar_uids(&self) -> Result<Vec<String>> {
        let reply = self
            .conn
            .call_method(
                Some(SOURCES),
                SOURCE_MANAGER,
                Some("org.freedesktop.DBus.ObjectManager"),
                "GetManagedObjects",
                &(),
            )
            .await?;
        let objects: ManagedObjects = reply.body().deserialize()?;
        let mut uids = vec![];
        for ifaces in objects.values() {
            let Some(props) = ifaces.get("org.gnome.evolution.dataserver.Source") else { continue };
            let get = |k: &str| props.get(k).and_then(|v| String::try_from(v.try_clone().ok()?).ok());
            if let (Some(uid), Some(data)) = (get("UID"), get("Data"))
                && is_enabled_calendar(&data)
            {
                uids.push(uid);
            }
        }
        uids.sort();
        Ok(uids)
    }

    async fn open(&mut self, uid: &str) -> Result<(String, String)> {
        if let Some(found) = self.opened.get(uid) {
            return Ok(found.clone());
        }
        let reply = self
            .conn
            .call_method(
                Some(FACTORY_BUS),
                FACTORY_PATH,
                Some("org.gnome.evolution.dataserver.CalendarFactory"),
                "OpenCalendar",
                &(uid,),
            )
            .await?;
        let (path, bus): (String, String) = reply.body().deserialize()?;
        self.conn.call_method(Some(bus.as_str()), path.as_str(), Some(CALENDAR_IFACE), "Open", &()).await?;
        self.opened.insert(uid.to_string(), (bus.clone(), path.clone()));
        Ok((bus, path))
    }

    async fn events_in(&mut self, uid: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<ical::VEvent>> {
        let (bus, path) = self.open(uid).await?;
        let fmt = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
        let query = format!("(occur-in-time-range? (make-time \"{}\") (make-time \"{}\"))", fmt(from), fmt(to));
        let reply = match self
            .conn
            .call_method(Some(bus.as_str()), path.as_str(), Some(CALENDAR_IFACE), "GetObjectList", &(query,))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // The backend may have gone away; reopen next time.
                self.opened.remove(uid);
                return Err(e.into());
            }
        };
        let objects: Vec<String> = reply.body().deserialize()?;
        Ok(objects.iter().flat_map(|o| ical::parse_vevents(o)).collect())
    }

    /// Events happening now and the rest of today, across every calendar.
    pub async fn state(&mut self, now: DateTime<Utc>) -> Result<CalendarState> {
        let local_now = now.with_timezone(&Local);
        let end_of_day = Local
            .from_local_datetime(&(local_now.date_naive() + Duration::days(1)).and_hms_opt(0, 0, 0).expect("midnight"))
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(now + Duration::hours(12));
        let from = now - Duration::hours(12);
        let mut events = vec![];
        let uids = self.calendar_uids().await?;
        for uid in &uids {
            match self.events_in(uid, from, end_of_day).await {
                Ok(mut e) => events.append(&mut e),
                Err(e) => debug!(uid, "calendar unavailable: {e}"),
            }
        }
        let mut state = CalendarState::default();
        for o in ical::occurrences(&events, from, end_of_day) {
            let event = CalendarEvent {
                summary: o.summary,
                start: o.start,
                end: o.end,
                location: o.location,
                all_day: o.all_day,
            };
            if o.start <= now && now < o.end {
                state.current.push(event);
            } else if o.start > now && o.start < end_of_day {
                state.upcoming.push(event);
            }
        }
        state.upcoming.truncate(5);
        if uids.is_empty() {
            return Err(CapsError::Other("no calendars configured".into()));
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_calendar_sources() {
        let personal = "\n[Data Source]\nDisplayName=Personal\nEnabled=true\nParent=local-stub\n\n[Calendar]\nBackendName=local\nColor=#62a0ea\n";
        let birthdays = "\n[Data Source]\nDisplayName=Birthdays\nEnabled=true\n\n[Calendar]\nBackendName=contacts\n";
        let disabled = "[Data Source]\nEnabled=false\n[Calendar]\nBackendName=caldav\n";
        let address_book = "[Data Source]\nEnabled=true\n[Address Book]\nBackendName=local\n";
        assert!(is_enabled_calendar(personal));
        assert!(!is_enabled_calendar(birthdays));
        assert!(!is_enabled_calendar(disabled));
        assert!(!is_enabled_calendar(address_book));
    }
}

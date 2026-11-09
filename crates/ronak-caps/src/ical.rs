//! Just enough iCalendar (RFC 5545) to answer "what is happening now and
//! next today": VEVENTs with start, end or duration, summary, location, time
//! zones, exclusions and the common recurrence rules.
//!
//! Supported recurrence: FREQ=DAILY, WEEKLY (with BYDAY), MONTHLY (same day
//! of month) and YEARLY, with INTERVAL, COUNT and UNTIL, plus EXDATE and
//! RECURRENCE-ID overrides. Rarer parts (BYSETPOS, BYMONTHDAY lists, ...)
//! are ignored, so such events may show up on extra days; that is acceptable
//! for a glance card and documented as a known limitation.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

#[derive(Debug, Clone, PartialEq)]
pub enum EventTime {
    Utc(DateTime<Utc>),
    Zoned(NaiveDateTime, Tz),
    /// No time zone: the user's local time.
    Floating(NaiveDateTime),
    /// A whole day (all-day events).
    Date(NaiveDate),
}

impl EventTime {
    fn naive(&self) -> NaiveDateTime {
        match self {
            EventTime::Utc(t) => t.naive_utc(),
            EventTime::Zoned(n, _) | EventTime::Floating(n) => *n,
            EventTime::Date(d) => d.and_hms_opt(0, 0, 0).expect("midnight exists"),
        }
    }

    /// The same wall-clock time zone, at another naive time.
    fn with_naive(&self, n: NaiveDateTime) -> EventTime {
        match self {
            EventTime::Utc(_) => EventTime::Utc(Utc.from_utc_datetime(&n)),
            EventTime::Zoned(_, tz) => EventTime::Zoned(n, *tz),
            EventTime::Floating(_) => EventTime::Floating(n),
            EventTime::Date(_) => EventTime::Date(n.date()),
        }
    }

    pub fn to_utc(&self) -> DateTime<Utc> {
        let local = |n: &NaiveDateTime| {
            Local
                .from_local_datetime(n)
                .earliest()
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|| Utc.from_utc_datetime(n))
        };
        match self {
            EventTime::Utc(t) => *t,
            EventTime::Zoned(n, tz) => tz
                .from_local_datetime(n)
                .earliest()
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|| Utc.from_utc_datetime(n)),
            EventTime::Floating(n) => local(n),
            EventTime::Date(d) => local(&d.and_hms_opt(0, 0, 0).expect("midnight exists")),
        }
    }

    pub fn is_date(&self) -> bool {
        matches!(self, EventTime::Date(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RRule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<EventTime>,
    pub by_day: Vec<Weekday>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VEvent {
    pub uid: String,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub start: EventTime,
    pub end: Option<EventTime>,
    pub duration: Option<Duration>,
    pub rrule: Option<RRule>,
    pub exdates: Vec<EventTime>,
    pub recurrence_id: Option<EventTime>,
    pub cancelled: bool,
}

/// One concrete occurrence of an event.
#[derive(Debug, Clone, PartialEq)]
pub struct Occurrence {
    pub summary: Option<String>,
    pub location: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
}

/// Joins folded lines (continuations start with a space or tab).
fn unfold(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    for raw in ics.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(cont) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t'))
            && let Some(last) = lines.last_mut()
        {
            last.push_str(cont);
            continue;
        }
        if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

/// "DTSTART;TZID=Europe/Rome:20261109T150000" -> ("DTSTART", {TZID: ..}, "20261109T150000").
fn property(line: &str) -> Option<(String, HashMap<String, String>, String)> {
    let colon = {
        // The first colon outside a quoted parameter value.
        let mut quoted = false;
        line.char_indices()
            .find(|(_, c)| {
                if *c == '"' {
                    quoted = !quoted;
                }
                *c == ':' && !quoted
            })?
            .0
    };
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_string()))
        .collect();
    Some((name, params, value.to_string()))
}

fn unescape(v: &str) -> String {
    v.replace("\\n", "\n").replace("\\N", "\n").replace("\\,", ",").replace("\\;", ";").replace("\\\\", "\\")
}

/// Resolves EDS-style zone ids such as "/freeassociation.sourceforge.net/Europe/Rome".
pub fn resolve_tz(tzid: &str) -> Option<Tz> {
    let segments: Vec<&str> = tzid.trim_matches('/').split('/').collect();
    (0..segments.len()).find_map(|i| segments[i..].join("/").parse::<Tz>().ok())
}

fn parse_time(value: &str, params: &HashMap<String, String>) -> Option<EventTime> {
    let value = value.trim();
    if params.get("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE")) || value.len() == 8 {
        return NaiveDate::parse_from_str(value, "%Y%m%d").ok().map(EventTime::Date);
    }
    if let Some(utc) = value.strip_suffix('Z') {
        let n = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some(EventTime::Utc(Utc.from_utc_datetime(&n)));
    }
    let n = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    match params.get("TZID").and_then(|t| resolve_tz(t)) {
        Some(tz) => Some(EventTime::Zoned(n, tz)),
        None => Some(EventTime::Floating(n)),
    }
}

/// ISO 8601 durations as used by iCalendar: "PT1H30M", "P1D", "-PT15M", "P1W".
pub fn parse_duration(v: &str) -> Option<Duration> {
    let (neg, rest) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let rest = rest.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut in_time = false;
    for c in rest.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (unit, in_time) {
                    ('W', false) => Duration::weeks(n),
                    ('D', false) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

fn weekday(code: &str) -> Option<Weekday> {
    // BYDAY may carry an ordinal ("1MO", "-1FR"); keep only the day.
    let day = code.trim_start_matches(|c: char| c == '+' || c == '-' || c.is_ascii_digit());
    Some(match day {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_rrule(v: &str) -> Option<RRule> {
    let mut rule = RRule { freq: Freq::Daily, interval: 1, count: None, until: None, by_day: vec![] };
    let mut freq = None;
    for part in v.split(';') {
        let (k, val) = part.split_once('=')?;
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match val {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None, // hourly and finer are not meetings
                })
            }
            "INTERVAL" => rule.interval = val.parse().ok().filter(|n| *n > 0)?,
            "COUNT" => rule.count = val.parse().ok(),
            "UNTIL" => rule.until = parse_time(val, &HashMap::new()),
            "BYDAY" => rule.by_day = val.split(',').filter_map(weekday).collect(),
            _ => {}
        }
    }
    rule.freq = freq?;
    Some(rule)
}

/// A property's parameters and value, e.g. `DTSTART;TZID=Europe/Rome:2026...`.
type PropValues = Vec<(HashMap<String, String>, String)>;
/// The properties of one VEVENT, by name.
type Props = HashMap<String, PropValues>;

/// Every VEVENT in a calendar object (a bare VEVENT or a whole VCALENDAR).
pub fn parse_vevents(ics: &str) -> Vec<VEvent> {
    let mut out = vec![];
    let mut current: Option<Props> = None;
    let mut depth = 0; // nested components such as VALARM
    for line in unfold(ics) {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VEVENT" {
            current = Some(HashMap::new());
            depth = 0;
            continue;
        }
        let Some(props) = current.as_mut() else { continue };
        if upper.starts_with("BEGIN:") {
            depth += 1;
            continue;
        }
        if upper.starts_with("END:") {
            if depth > 0 {
                depth -= 1;
                continue;
            }
            if upper == "END:VEVENT"
                && let Some(ev) = current.take().and_then(build_event)
            {
                out.push(ev);
            }
            continue;
        }
        if depth > 0 {
            continue;
        }
        if let Some((name, params, value)) = property(&line) {
            props.entry(name).or_default().push((params, value));
        }
    }
    out
}

fn build_event(props: Props) -> Option<VEvent> {
    let first = |k: &str| props.get(k).and_then(|v| v.first());
    let time = |k: &str| first(k).and_then(|(p, v)| parse_time(v, p));
    let start = time("DTSTART")?;
    let mut exdates = vec![];
    for (p, v) in props.get("EXDATE").into_iter().flatten() {
        for one in v.split(',') {
            if let Some(t) = parse_time(one, p) {
                exdates.push(t);
            }
        }
    }
    Some(VEvent {
        uid: first("UID").map(|(_, v)| v.clone()).unwrap_or_default(),
        summary: first("SUMMARY").map(|(_, v)| unescape(v)).filter(|s| !s.is_empty()),
        location: first("LOCATION").map(|(_, v)| unescape(v)).filter(|s| !s.is_empty()),
        end: time("DTEND"),
        duration: first("DURATION").and_then(|(_, v)| parse_duration(v)),
        rrule: first("RRULE").and_then(|(_, v)| parse_rrule(v)),
        recurrence_id: time("RECURRENCE-ID"),
        cancelled: first("STATUS").is_some_and(|(_, v)| v.eq_ignore_ascii_case("CANCELLED")),
        exdates,
        start,
    })
}

impl VEvent {
    fn length(&self) -> Duration {
        if let Some(end) = &self.end {
            return end.naive() - self.start.naive();
        }
        self.duration.unwrap_or(if self.start.is_date() { Duration::days(1) } else { Duration::zero() })
    }

    /// Start times (in the event's own zone) of every occurrence that could
    /// overlap [from, to].
    fn starts(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<EventTime> {
        let Some(rule) = &self.rrule else { return vec![self.start.clone()] };
        let first = self.start.naive();
        let len = self.length();
        let until = rule.until.as_ref().map(|u| u.to_utc());
        let mut out = vec![];
        let mut produced = 0u32;
        let step = rule.interval as i64;
        let candidates_in_period = |period_start: NaiveDateTime| -> Vec<NaiveDateTime> {
            match rule.freq {
                Freq::Weekly if !rule.by_day.is_empty() => {
                    // The Monday of the period's week, then each listed day.
                    let monday =
                        period_start.date() - Duration::days(period_start.weekday().num_days_from_monday() as i64);
                    let mut days: Vec<NaiveDateTime> = rule
                        .by_day
                        .iter()
                        .map(|d| (monday + Duration::days(d.num_days_from_monday() as i64)).and_time(first.time()))
                        .collect();
                    days.sort();
                    days
                }
                _ => vec![period_start],
            }
        };
        for i in 0..2000i64 {
            let period_start = match rule.freq {
                Freq::Daily => first + Duration::days(i * step),
                Freq::Weekly => first + Duration::weeks(i * step),
                Freq::Monthly => match add_months(first, (i * step) as i32) {
                    Some(t) => t,
                    None => continue, // e.g. the 31st in a 30-day month
                },
                Freq::Yearly => match add_months(first, (i * step * 12) as i32) {
                    Some(t) => t,
                    None => continue,
                },
            };
            for start in candidates_in_period(period_start) {
                if start < first {
                    continue;
                }
                let t = self.start.with_naive(start);
                let utc = t.to_utc();
                if until.is_some_and(|u| utc > u) || rule.count.is_some_and(|c| produced >= c) {
                    return out;
                }
                produced += 1;
                if utc > to {
                    return out;
                }
                if utc + len >= from && !self.exdates.iter().any(|x| x.to_utc() == utc) {
                    out.push(t);
                }
            }
        }
        out
    }
}

fn add_months(t: NaiveDateTime, months: i32) -> Option<NaiveDateTime> {
    let total = t.year() * 12 + t.month0() as i32 + months;
    let date = NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, t.day())?;
    Some(date.and_time(t.time()))
}

/// Concrete occurrences overlapping [from, to], honoring overrides
/// (RECURRENCE-ID) and cancellations, sorted by start.
pub fn occurrences(events: &[VEvent], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Occurrence> {
    // Instances moved or cancelled individually replace the master's.
    let overridden: Vec<(String, DateTime<Utc>)> =
        events.iter().filter_map(|e| e.recurrence_id.as_ref().map(|r| (e.uid.clone(), r.to_utc()))).collect();
    let mut out = vec![];
    for e in events {
        if e.cancelled {
            continue;
        }
        let len = e.length();
        for start in e.starts(from, to) {
            let s = start.to_utc();
            if e.recurrence_id.is_none() && overridden.iter().any(|(uid, at)| *uid == e.uid && *at == s) {
                continue;
            }
            let end = start.with_naive(start.naive() + len).to_utc();
            if end > from && s < to || (len.is_zero() && s >= from && s <= to) {
                out.push(Occurrence {
                    summary: e.summary.clone(),
                    location: e.location.clone(),
                    start: s,
                    end,
                    all_day: e.start.is_date(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.summary.cmp(&b.summary)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn single_event_and_folding() {
        let ics = "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261109T140000Z\r\nDTEND:20261109T150000Z\r\nSUMMARY:Design \r\n review\\, final\r\nLOCATION:Room 4\r\nEND:VEVENT\r\n";
        let evs = parse_vevents(ics);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].summary.as_deref(), Some("Design review, final"));
        let occ = occurrences(&evs, utc("2026-11-09T00:00:00Z"), utc("2026-11-10T00:00:00Z"));
        assert_eq!(occ.len(), 1);
        assert_eq!((occ[0].start, occ[0].end), (utc("2026-11-09T14:00:00Z"), utc("2026-11-09T15:00:00Z")));
    }

    #[test]
    fn zones_including_eds_style_ids() {
        assert_eq!(resolve_tz("Europe/Rome"), Some(chrono_tz::Europe::Rome));
        assert_eq!(resolve_tz("/freeassociation.sourceforge.net/Europe/Rome"), Some(chrono_tz::Europe::Rome));
        assert_eq!(
            resolve_tz("/freeassociation.sourceforge.net/Tzfile/America/New_York"),
            Some(chrono_tz::America::New_York)
        );
        let ics = "BEGIN:VEVENT\nUID:b\nDTSTART;TZID=/freeassociation.sourceforge.net/Europe/Rome:20261109T150000\nDURATION:PT30M\nEND:VEVENT";
        let occ = occurrences(&parse_vevents(ics), utc("2026-11-09T00:00:00Z"), utc("2026-11-10T00:00:00Z"));
        // 15:00 in Rome in November (UTC+1) is 14:00 UTC.
        assert_eq!((occ[0].start, occ[0].end), (utc("2026-11-09T14:00:00Z"), utc("2026-11-09T14:30:00Z")));
    }

    #[test]
    fn weekly_standup_with_exception_and_override() {
        let ics = "BEGIN:VEVENT\nUID:s\nDTSTART;TZID=Europe/Rome:20260105T093000\nDTEND;TZID=Europe/Rome:20260105T094500\n\
                   RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\nEXDATE;TZID=Europe/Rome:20261111T093000\nSUMMARY:Standup\nEND:VEVENT\n\
                   BEGIN:VEVENT\nUID:s\nRECURRENCE-ID;TZID=Europe/Rome:20261113T093000\nDTSTART;TZID=Europe/Rome:20261113T110000\n\
                   DTEND;TZID=Europe/Rome:20261113T111500\nSUMMARY:Standup (moved)\nEND:VEVENT";
        let evs = parse_vevents(ics);
        let occ = occurrences(&evs, utc("2026-11-09T00:00:00Z"), utc("2026-11-15T00:00:00Z"));
        let summary: Vec<(String, String)> =
            occ.iter().map(|o| (o.start.to_rfc3339(), o.summary.clone().unwrap())).collect();
        assert_eq!(
            summary,
            vec![
                ("2026-11-09T08:30:00+00:00".to_string(), "Standup".to_string()),
                ("2026-11-13T10:00:00+00:00".to_string(), "Standup (moved)".to_string()),
            ],
            "Monday stays, Wednesday is excluded, Friday was moved to 11:00"
        );
    }

    #[test]
    fn daylight_saving_keeps_wall_clock_time() {
        // A daily 09:00 Rome meeting is 08:00 UTC in winter and 07:00 UTC in summer.
        let ics = "BEGIN:VEVENT\nUID:d\nDTSTART;TZID=Europe/Rome:20260101T090000\nDURATION:PT1H\nRRULE:FREQ=DAILY\nEND:VEVENT";
        let evs = parse_vevents(ics);
        let winter = occurrences(&evs, utc("2026-02-02T00:00:00Z"), utc("2026-02-03T00:00:00Z"));
        let summer = occurrences(&evs, utc("2026-07-02T00:00:00Z"), utc("2026-07-03T00:00:00Z"));
        assert_eq!(winter[0].start, utc("2026-02-02T08:00:00Z"));
        assert_eq!(summer[0].start, utc("2026-07-02T07:00:00Z"));
    }

    #[test]
    fn counts_until_all_day_and_cancelled() {
        let ics = "BEGIN:VEVENT\nUID:c\nDTSTART:20261101T100000Z\nDURATION:PT1H\nRRULE:FREQ=DAILY;COUNT=3\nEND:VEVENT\n\
                   BEGIN:VEVENT\nUID:u\nDTSTART:20261101T120000Z\nDURATION:PT1H\nRRULE:FREQ=DAILY;INTERVAL=2;UNTIL=20261105T120000Z\nEND:VEVENT\n\
                   BEGIN:VEVENT\nUID:h\nDTSTART;VALUE=DATE:20261109\nDTEND;VALUE=DATE:20261110\nSUMMARY:Holiday\nEND:VEVENT\n\
                   BEGIN:VEVENT\nUID:x\nDTSTART:20261109T100000Z\nDURATION:PT1H\nSTATUS:CANCELLED\nEND:VEVENT\n\
                   BEGIN:VEVENT\nUID:m\nDTSTART:20260131T100000Z\nDURATION:PT1H\nRRULE:FREQ=MONTHLY\nEND:VEVENT";
        let evs = parse_vevents(ics);
        let occ = |a: &str, b: &str| occurrences(&evs, utc(a), utc(b)).into_iter().filter(|o| !o.all_day).count();
        assert_eq!(
            occ("2026-11-01T00:00:00Z", "2026-11-30T00:00:00Z"),
            3 + 3,
            "COUNT=3; UNTIL with interval 2 gives 1st, 3rd, 5th"
        );
        let all_day = occurrences(&evs, utc("2026-11-09T00:00:00Z"), utc("2026-11-10T00:00:00Z"));
        assert!(all_day.iter().any(|o| o.all_day && o.summary.as_deref() == Some("Holiday")));
        assert!(!all_day.iter().any(|o| o.start == utc("2026-11-09T10:00:00Z")), "cancelled events are skipped");
        assert_eq!(
            occ("2026-02-01T00:00:00Z", "2026-03-30T00:00:00Z"),
            0,
            "the 31st is skipped in months without one (and March 31 is after the range)"
        );
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1D"), Some(Duration::days(1)));
        assert_eq!(parse_duration("P1W"), Some(Duration::weeks(1)));
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
        assert_eq!(parse_duration("1H"), None);
    }
}

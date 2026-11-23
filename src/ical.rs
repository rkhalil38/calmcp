//! A small, lossless iCalendar (RFC 5545) reader/editor.
//!
//! Resources are parsed into a tree of blocks (`BEGIN:X` ... `END:X`) holding
//! raw content lines. Edits replace whole lines on the targeted VEVENT and
//! leave every other line untouched, so properties, parameters and components
//! we don't understand (VALARM, VTIMEZONE, X- properties, ...) survive updates.

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp, ZonedDateTime};

#[derive(Debug)]
pub struct Calendar {
    pub href: String,
    pub name: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct Event {
    pub id: String,   // href, passed back by the agent
    pub etag: String, // for If-Match on update/delete
    pub uid: String,
    pub summary: String,
    pub time: EventTime,
    pub location: Option<String>,
    pub description: Option<String>,
    pub recurring: bool,
    /// Original start of this occurrence (RECURRENCE-ID), identifying it within the series.
    pub recurrence_id: Option<String>,
    /// Whether the event blocks time. Cancelled and TRANSP:TRANSPARENT events
    /// don't; all-day events only do when marked TRANSP:OPAQUE.
    pub busy: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventTime {
    /// End is exclusive (an event on Nov 20 ends Nov 21).
    AllDay { start: Date, end: Date },
    Timed {
        start: ZonedDateTime,
        end: ZonedDateTime,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum IcalError {
    #[error("not a valid iCalendar object: {0}")]
    InvalidIcs(&'static str),
    #[error("event has no DTSTART")]
    MissingStart,
    #[error("start and end must both be dates or both be date-times")]
    MixedValueTypes,
    #[error("end must be after start")]
    EndBeforeStart,
    #[error("invalid DURATION: {0}")]
    InvalidDuration(String),
    #[error("summary must not be empty")]
    EmptySummary,
    #[error("resource contains no event to edit")]
    NoEvent,
    #[error("no occurrence with recurrence_id {0}")]
    OccurrenceNotFound(String),
    #[error("invalid date/time: {0}")]
    Time(#[from] jiff::Error),
}

/// A DTSTART/DTEND value: a DATE (all-day) or a DATE-TIME.
#[derive(Clone, Debug)]
pub enum Time {
    Date(Date),
    Zoned(ZonedDateTime),
}

impl Time {
    /// Parse a time given by the agent: "2026-12-01" (all-day),
    /// "2026-12-01T15:00:00-05:00" (RFC 3339), a zoned time as returned by
    /// `list_events`, or "2026-12-01T15:00" (local time in `tz`).
    pub fn parse_input(s: &str, tz: &TimeZone) -> Result<Time, jiff::Error> {
        if !s.contains('T') {
            return Ok(Time::Date(s.parse()?));
        }
        if let Ok(zdt) = s.parse::<ZonedDateTime>() {
            return Ok(Time::Zoned(zdt.with_time_zone(tz.clone())));
        }
        if let Ok(ts) = s.parse::<Timestamp>() {
            return Ok(Time::Zoned(ts.to_zoned_datetime(tz.clone())));
        }
        Ok(Time::Zoned(
            s.parse::<DateTime>()?.to_zoned_datetime(tz.clone())?,
        ))
    }

    /// The instant this time starts at; a date starts at midnight in `tz`.
    pub fn timestamp(&self, tz: &TimeZone) -> Result<Timestamp, jiff::Error> {
        match self {
            Time::Date(d) => Ok(d.to_zoned_datetime(tz.clone())?.timestamp()),
            Time::Zoned(zdt) => Ok(zdt.timestamp()),
        }
    }

    fn same_instant(&self, other: &Time) -> bool {
        match (self, other) {
            (Time::Date(a), Time::Date(b)) => a == b,
            (Time::Zoned(a), Time::Zoned(b)) => a.timestamp() == b.timestamp(),
            _ => false,
        }
    }
}

/// Fields to set on an event. `None` leaves a field unchanged; for location
/// and description, `Some("")` removes it.
#[derive(Default)]
pub struct EventFields {
    pub summary: Option<String>,
    pub start: Option<Time>,
    pub end: Option<Time>,
    pub location: Option<String>,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Parse one CalDAV resource (.ics text) into events.
///
/// Expects the server to have expanded recurrences (CalDAV `<C:expand>`), so
/// each VEVENT is a single occurrence with no RRULE. `tz` is used for floating
/// times and for the returned times.
pub fn parse_events(
    href: &str,
    etag: &str,
    ics: &str,
    tz: &TimeZone,
) -> Result<Vec<Event>, IcalError> {
    let cal = parse_calendar(ics)?;

    let mut events = Vec::new();
    for ev in cal.blocks("VEVENT") {
        let (start, end) = event_times(ev, tz)?;
        let time = match (start, end) {
            (Time::Date(start), Time::Date(end)) => EventTime::AllDay { start, end },
            (Time::Zoned(start), Time::Zoned(end)) => EventTime::Timed { start, end },
            _ => return Err(IcalError::MixedValueTypes),
        };

        let cancelled = ev
            .prop("STATUS")
            .is_some_and(|p| p.value.eq_ignore_ascii_case("CANCELLED"));
        let busy = !cancelled
            && match ev.prop("TRANSP") {
                Some(p) => p.value.eq_ignore_ascii_case("OPAQUE"),
                None => matches!(time, EventTime::Timed { .. }),
            };

        let recurrence_id = ev.prop("RECURRENCE-ID").map(|p| p.value.to_string());
        events.push(Event {
            id: href.to_string(),
            etag: etag.to_string(),
            uid: ev
                .prop("UID")
                .map(|p| p.value.to_string())
                .unwrap_or_default(),
            summary: ev.text("SUMMARY").unwrap_or_else(|| "(untitled)".into()),
            time,
            location: ev.text("LOCATION"),
            description: ev.text("DESCRIPTION"),
            recurring: recurrence_id.is_some(),
            recurrence_id,
            busy,
        });
    }
    Ok(events)
}

/// An event's start and end. A missing end follows RFC 5545 3.6.1: one day
/// for a DATE, zero length for a DATE-TIME.
fn event_times(ev: &Block, tz: &TimeZone) -> Result<(Time, Time), IcalError> {
    let start = ev
        .prop("DTSTART")
        .ok_or(IcalError::MissingStart)?
        .time(tz)?;
    let end = match (ev.prop("DTEND"), ev.prop("DURATION")) {
        (Some(p), _) => p.time(tz)?,
        (None, Some(d)) => {
            let span: Span = d
                .value
                .parse()
                .map_err(|_| IcalError::InvalidDuration(d.value.to_string()))?;
            match &start {
                Time::Date(date) => Time::Date(date.checked_add(span)?),
                Time::Zoned(zdt) => Time::Zoned(zdt.checked_add(span)?),
            }
        }
        (None, None) => match &start {
            Time::Date(date) => Time::Date(date.tomorrow()?),
            Time::Zoned(zdt) => Time::Zoned(zdt.clone()),
        },
    };
    Ok((start, end))
}

/// The instant a recurrence_id (as returned by `list_events`) refers to. An
/// all-day occurrence maps to midnight UTC on its date.
pub fn recurrence_instant(recurrence_id: &str) -> Result<Timestamp, IcalError> {
    Ok(match parse_value(recurrence_id, None, &TimeZone::UTC)? {
        Time::Date(d) => d.to_zoned_datetime(TimeZone::UTC)?.timestamp(),
        Time::Zoned(zdt) => zdt.timestamp(),
    })
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Build a new single-event VCALENDAR. `fields` must have summary, start and end.
pub fn new_event(uid: &str, fields: &EventFields, tz: &TimeZone) -> Result<String, IcalError> {
    let mut ev = Block::new("VEVENT");
    ev.set("UID", format!("UID:{}", escape_text(uid)));
    apply_fields(&mut ev, fields, tz)?;

    let mut cal = Block::new("VCALENDAR");
    cal.set("VERSION", "VERSION:2.0".into());
    cal.set("PRODID", "PRODID:-//calmcp//calmcp//EN".into());
    cal.lines.push(Line::Block(ev));
    Ok(cal.render())
}

/// Apply `fields` to an event in an existing resource, returning the new .ics.
///
/// Without `recurrence_id`, edits the event itself (for a recurring event,
/// the whole series). With it, edits only that occurrence, adding an override
/// VEVENT to the resource if the occurrence doesn't have one yet.
pub fn update_event(
    ics: &str,
    recurrence_id: Option<&str>,
    fields: &EventFields,
    tz: &TimeZone,
) -> Result<String, IcalError> {
    let mut cal = parse_calendar(ics)?;

    let target = match recurrence_id {
        None => cal
            .lines
            .iter()
            .position(|l| matches!(l, Line::Block(b) if b.name == "VEVENT" && b.prop("RECURRENCE-ID").is_none()))
            .ok_or(IcalError::NoEvent)?,
        Some(rid) => {
            let wanted = parse_value(rid, None, tz)?;
            let existing = cal.lines.iter().position(|l| {
                matches!(l, Line::Block(b) if b.name == "VEVENT"
                    && b.prop("RECURRENCE-ID")
                        .and_then(|p| p.time(tz).ok())
                        .is_some_and(|t| t.same_instant(&wanted)))
            });
            match existing {
                Some(i) => i,
                None => {
                    let master = cal
                        .blocks("VEVENT")
                        .find(|b| b.prop("RECURRENCE-ID").is_none() && b.prop("RRULE").is_some())
                        .ok_or_else(|| IcalError::OccurrenceNotFound(rid.to_string()))?;
                    let occurrence = override_for(master, &wanted, tz)?;
                    cal.lines.push(Line::Block(occurrence));
                    cal.lines.len() - 1
                }
            }
        }
    };

    let Line::Block(ev) = &mut cal.lines[target] else {
        unreachable!()
    };
    apply_fields(ev, fields, tz)?;
    bump_sequence(ev);
    Ok(cal.render())
}

/// Cancel one occurrence of a recurring event: exclude it from the series
/// (EXDATE) and drop any override VEVENT for it. Returns `None` if no event
/// would be left in the resource, in which case it should be deleted instead.
pub fn cancel_occurrence(
    ics: &str,
    recurrence_id: &str,
    tz: &TimeZone,
) -> Result<Option<String>, IcalError> {
    let mut cal = parse_calendar(ics)?;
    let wanted = parse_value(recurrence_id, None, tz)?;

    cal.lines.retain(|l| {
        !matches!(l, Line::Block(b) if b.name == "VEVENT"
            && b.prop("RECURRENCE-ID")
                .and_then(|p| p.time(tz).ok())
                .is_some_and(|t| t.same_instant(&wanted)))
    });

    if cal.blocks("VEVENT").next().is_none() {
        return Ok(None);
    }

    let master = cal.lines.iter_mut().find_map(|l| match l {
        Line::Block(b) if b.name == "VEVENT" && b.prop("RECURRENCE-ID").is_none() => Some(b),
        _ => None,
    });
    if let Some(master) = master {
        let form = master
            .prop("DTSTART")
            .ok_or(IcalError::MissingStart)?
            .form();
        master.add(format_time("EXDATE", &wanted, &form, tz)?);
        apply_fields(master, &EventFields::default(), tz)?;
        bump_sequence(master);
    }
    Ok(Some(cal.render()))
}

/// A new override VEVENT for one occurrence of `master`: a copy of the master
/// without its recurrence rules, moved to the occurrence's start.
fn override_for(master: &Block, occurrence: &Time, tz: &TimeZone) -> Result<Block, IcalError> {
    const DROP: [&str; 6] = [
        "RRULE",
        "RDATE",
        "EXDATE",
        "DTSTART",
        "DTEND",
        "RECURRENCE-ID",
    ];

    let form = master
        .prop("DTSTART")
        .ok_or(IcalError::MissingStart)?
        .form();
    let (start, end) = event_times(master, tz)?;

    let mut ev = Block::new("VEVENT");
    ev.lines = master
        .lines
        .iter()
        .filter(|l| !matches!(l, Line::Prop(p) if DROP.iter().any(|d| prop_name(p).eq_ignore_ascii_case(d))))
        .cloned()
        .collect();

    ev.set(
        "RECURRENCE-ID",
        format_time("RECURRENCE-ID", occurrence, &form, tz)?,
    );
    ev.set("DTSTART", format_time("DTSTART", occurrence, &form, tz)?);
    if master.prop("DTEND").is_some() {
        let occ_end = shift(&end, &start, occurrence)?;
        ev.set("DTEND", format_time("DTEND", &occ_end, &form, tz)?);
    }
    Ok(ev)
}

fn apply_fields(ev: &mut Block, fields: &EventFields, tz: &TimeZone) -> Result<(), IcalError> {
    if let Some(summary) = &fields.summary {
        if summary.trim().is_empty() {
            return Err(IcalError::EmptySummary);
        }
        ev.set("SUMMARY", format!("SUMMARY:{}", escape_text(summary)));
    }
    for (name, value) in [
        ("LOCATION", &fields.location),
        ("DESCRIPTION", &fields.description),
    ] {
        match value.as_deref() {
            None => {}
            Some("") => ev.remove(name),
            Some(v) => ev.set(name, format!("{name}:{}", escape_text(v))),
        }
    }

    if fields.start.is_some() || fields.end.is_some() {
        let form = ev.prop("DTSTART").map(|p| p.form()).unwrap_or(Form::Utc);
        let old = match ev.prop("DTSTART") {
            Some(_) => Some(event_times(ev, tz)?),
            None => None,
        };

        let start = match (&fields.start, &old) {
            (Some(s), _) => s.clone(),
            (None, Some((s, _))) => s.clone(),
            (None, None) => return Err(IcalError::MissingStart),
        };
        // Without a new end, keep the event's length.
        let end = match (&fields.end, &old) {
            (Some(e), _) => e.clone(),
            (None, Some((old_start, old_end))) => shift(old_end, old_start, &start)?,
            (None, None) => return Err(IcalError::MissingStart),
        };
        match (&start, &end) {
            (Time::Date(s), Time::Date(e)) if e <= s => return Err(IcalError::EndBeforeStart),
            (Time::Zoned(s), Time::Zoned(e)) if e.timestamp() < s.timestamp() => {
                return Err(IcalError::EndBeforeStart);
            }
            (Time::Date(_), Time::Zoned(_)) | (Time::Zoned(_), Time::Date(_)) => {
                return Err(IcalError::MixedValueTypes);
            }
            _ => {}
        }

        ev.set("DTSTART", format_time("DTSTART", &start, &form, tz)?);
        ev.remove("DURATION");
        ev.set("DTEND", format_time("DTEND", &end, &form, tz)?);
    }

    let now = Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string();
    ev.set("DTSTAMP", format!("DTSTAMP:{now}"));
    ev.set("LAST-MODIFIED", format!("LAST-MODIFIED:{now}"));
    Ok(())
}

fn bump_sequence(ev: &mut Block) {
    let seq = ev
        .prop("SEQUENCE")
        .and_then(|p| p.value.parse::<u32>().ok())
        .unwrap_or(0);
    ev.set("SEQUENCE", format!("SEQUENCE:{}", seq + 1));
}

/// Move `end` by however far `new_start` is from `old_start`.
fn shift(end: &Time, old_start: &Time, new_start: &Time) -> Result<Time, IcalError> {
    Ok(match (end, old_start, new_start) {
        (Time::Date(end), Time::Date(old), Time::Date(new)) => {
            Time::Date(end.checked_add(new.since(*old)?)?)
        }
        (Time::Zoned(end), Time::Zoned(old), Time::Zoned(new)) => {
            Time::Zoned(new.checked_add(end.duration_since(old))?)
        }
        _ => return Err(IcalError::MixedValueTypes),
    })
}

/// How an existing DATE-TIME is written, so edits keep the same form.
#[derive(Clone)]
enum Form {
    Utc,
    Floating,
    Tzid(String),
}

fn format_time(name: &str, time: &Time, form: &Form, tz: &TimeZone) -> Result<String, IcalError> {
    let zdt = match time {
        Time::Date(d) => return Ok(format!("{name};VALUE=DATE:{}", d.strftime("%Y%m%d"))),
        Time::Zoned(zdt) => zdt,
    };
    Ok(match form {
        Form::Tzid(tzid) if TimeZone::get(tzid).is_ok() => {
            let local = zdt.with_time_zone(TimeZone::get(tzid)?).datetime();
            format!(
                "{name};TZID={}:{}",
                quote_param(tzid),
                local.strftime("%Y%m%dT%H%M%S")
            )
        }
        Form::Floating => format!(
            "{name}:{}",
            zdt.with_time_zone(tz.clone())
                .datetime()
                .strftime("%Y%m%dT%H%M%S")
        ),
        _ => format!("{name}:{}", zdt.timestamp().strftime("%Y%m%dT%H%M%SZ")),
    })
}

// ---------------------------------------------------------------------------
// Content lines
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Line {
    /// An unfolded content line, kept verbatim.
    Prop(String),
    Block(Block),
}

#[derive(Clone, Debug)]
struct Block {
    name: String,
    lines: Vec<Line>,
}

/// A borrowed view of one content line: `NAME;PARAM=x:value`.
struct Prop<'a> {
    params: Vec<(&'a str, &'a str)>,
    value: &'a str,
}

fn parse_calendar(ics: &str) -> Result<Block, IcalError> {
    let unfolded = ics
        .replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "")
        .replace("\n\t", "");

    let mut stack = vec![Block::new("")];
    for line in unfolded.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(name) = strip_prefix_ci(line, "BEGIN:") {
            stack.push(Block::new(&name.trim().to_ascii_uppercase()));
        } else if let Some(name) = strip_prefix_ci(line, "END:") {
            let block = stack
                .pop()
                .filter(|b| b.name.eq_ignore_ascii_case(name.trim()));
            let (Some(block), Some(parent)) = (block, stack.last_mut()) else {
                return Err(IcalError::InvalidIcs("mismatched BEGIN/END"));
            };
            parent.lines.push(Line::Block(block));
        } else {
            let current = stack
                .last_mut()
                .ok_or(IcalError::InvalidIcs("content after END"))?;
            current.lines.push(Line::Prop(line.to_string()));
        }
    }

    let mut root = stack
        .pop()
        .filter(|_| stack.is_empty())
        .ok_or(IcalError::InvalidIcs("unterminated BEGIN"))?;
    match root.lines.pop() {
        Some(Line::Block(cal)) if cal.name == "VCALENDAR" && root.lines.is_empty() => Ok(cal),
        _ => Err(IcalError::InvalidIcs("expected a single VCALENDAR")),
    }
}

impl Block {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            lines: Vec::new(),
        }
    }

    fn blocks<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Block> {
        self.lines.iter().filter_map(move |l| match l {
            Line::Block(b) if b.name == name => Some(b),
            _ => None,
        })
    }

    /// The first top-level property with this name.
    fn prop(&self, name: &str) -> Option<Prop<'_>> {
        self.lines.iter().find_map(|l| match l {
            Line::Prop(p) if prop_name(p).eq_ignore_ascii_case(name) => parse_prop(p),
            _ => None,
        })
    }

    /// A text property's unescaped value, if present and non-empty.
    fn text(&self, name: &str) -> Option<String> {
        self.prop(name)
            .map(|p| unescape_text(p.value))
            .filter(|s| !s.is_empty())
    }

    /// Replace the property `name` with `line`, or add it before any child blocks.
    fn set(&mut self, name: &str, line: String) {
        let is_name =
            |l: &Line| matches!(l, Line::Prop(p) if prop_name(p).eq_ignore_ascii_case(name));
        match self.lines.iter().position(is_name) {
            Some(i) => {
                self.lines[i] = Line::Prop(line);
                let mut seen = false;
                self.lines
                    .retain(|l| !is_name(l) || !std::mem::replace(&mut seen, true));
            }
            None => self.add(line),
        }
    }

    /// Add a property line, alongside any others with the same name.
    fn add(&mut self, line: String) {
        let at = self
            .lines
            .iter()
            .position(|l| matches!(l, Line::Block(_)))
            .unwrap_or(self.lines.len());
        self.lines.insert(at, Line::Prop(line));
    }

    fn remove(&mut self, name: &str) {
        self.lines
            .retain(|l| !matches!(l, Line::Prop(p) if prop_name(p).eq_ignore_ascii_case(name)));
    }

    fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out);
        out
    }

    fn render_into(&self, out: &mut String) {
        fold_into(out, &format!("BEGIN:{}", self.name));
        for line in &self.lines {
            match line {
                Line::Prop(p) => fold_into(out, p),
                Line::Block(b) => b.render_into(out),
            }
        }
        fold_into(out, &format!("END:{}", self.name));
    }
}

impl Prop<'_> {
    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| *v)
    }

    fn time(&self, tz: &TimeZone) -> Result<Time, IcalError> {
        parse_value(self.value, self.param("TZID"), tz)
    }

    fn form(&self) -> Form {
        match self.param("TZID") {
            Some(tzid) => Form::Tzid(tzid.to_string()),
            None if self.value.contains('T') && !self.value.ends_with('Z') => Form::Floating,
            None => Form::Utc, // UTC, or a DATE (a new DATE-TIME is then written in UTC)
        }
    }
}

/// Parse a DATE or DATE-TIME value, converting date-times into `tz`.
fn parse_value(value: &str, tzid: Option<&str>, tz: &TimeZone) -> Result<Time, IcalError> {
    // Go by the value's shape rather than the VALUE=DATE parameter: Radicale
    // omits that parameter on all-day occurrences in expanded results.
    if !value.contains('T') {
        return Ok(Time::Date(Date::strptime("%Y%m%d", value)?));
    }

    let (civil, zone) = match (value.strip_suffix('Z'), tzid) {
        (Some(v), _) => (v, TimeZone::UTC),
        (None, Some(tzid)) => (value, TimeZone::get(tzid)?),
        (None, None) => (value, tz.clone()), // floating time
    };
    let zdt = DateTime::strptime("%Y%m%dT%H%M%S", civil)?.to_zoned_datetime(zone)?;
    Ok(Time::Zoned(zdt.with_time_zone(tz.clone())))
}

fn prop_name(line: &str) -> &str {
    line.split([';', ':']).next().unwrap_or_default()
}

/// Split a content line into its parameters and value, respecting quoted
/// parameter values (which may contain ':' and ';').
fn parse_prop(line: &str) -> Option<Prop<'_>> {
    let mut in_quotes = false;
    let mut params = Vec::new();
    let mut seg_start = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' | ':' if !in_quotes => {
                if let Some(s) = seg_start {
                    let (k, v) = line[s..i].split_once('=').unwrap_or((&line[s..i], ""));
                    params.push((k, v.trim_matches('"')));
                }
                if c == ':' {
                    return Some(Prop {
                        params,
                        value: &line[i + 1..],
                    });
                }
                seg_start = Some(i + 1);
            }
            _ => {}
        }
    }
    None
}

fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.get(..prefix.len())
        .filter(|p| p.eq_ignore_ascii_case(prefix))
        .map(|_| &line[prefix.len()..])
}

/// Append `line` folded at 75 octets (RFC 5545 3.1), with CRLF.
fn fold_into(out: &mut String, line: &str) {
    let mut width = 0;
    for c in line.chars() {
        if width + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out.push_str("\r\n");
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(c) => out.push(c),
            None => out.push('\\'),
        }
    }
    out
}

fn quote_param(value: &str) -> String {
    if value.contains([';', ':', ',']) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny() -> TimeZone {
        TimeZone::get("America/New_York").unwrap()
    }

    const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\n\
BEGIN:VTIMEZONE\r\nTZID:America/New_York\r\nBEGIN:STANDARD\r\nDTSTART:19701101T020000\r\n\
TZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:weekly@test\r\nDTSTAMP:20261001T000000Z\r\n\
DTSTART;TZID=America/New_York:20261005T090000\r\nDTEND;TZID=America/New_York:20261005T093000\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO\r\nSUMMARY:Weekly sync\r\nX-CUSTOM;X-P=\"a:b\":keep me\r\n\
DESCRIPTION:line one\\, with comma\\nline two\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn roundtrip_is_lossless() {
        assert_eq!(parse_calendar(WEEKLY).unwrap().render(), WEEKLY);
    }

    #[test]
    fn reads_text_and_all_day_without_value_date() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261027\r\nDTEND:20261028\r\n\
SUMMARY:Gym\\, day\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let ev = &parse_events("/c/a.ics", "e", ics, &ny()).unwrap()[0];
        assert_eq!(ev.summary, "Gym, day");
        assert!(matches!(ev.time, EventTime::AllDay { .. }));
    }

    #[test]
    fn series_edit_keeps_tzid_length_and_unknown_lines() {
        let fields = EventFields {
            summary: Some("Sync, renamed".into()),
            start: Some(Time::parse_input("2026-10-05T10:00", &ny()).unwrap()),
            ..Default::default()
        };
        let out = update_event(WEEKLY, None, &fields, &ny()).unwrap();
        assert!(out.contains("DTSTART;TZID=America/New_York:20261005T100000\r\n"));
        assert!(out.contains("DTEND;TZID=America/New_York:20261005T103000\r\n"));
        assert!(out.contains("SUMMARY:Sync\\, renamed\r\n"));
        assert!(out.contains("X-CUSTOM;X-P=\"a:b\":keep me\r\n"));
        assert!(out.contains("DESCRIPTION:line one\\, with comma\\nline two\r\n"));
        assert!(out.contains("BEGIN:VALARM\r\n"));
        assert!(out.contains("RRULE:FREQ=WEEKLY;BYDAY=MO\r\n"));
        assert!(out.contains("SEQUENCE:1\r\n"));
    }

    #[test]
    fn occurrence_edit_adds_override() {
        let fields = EventFields {
            summary: Some("Moved".into()),
            ..Default::default()
        };
        // 2026-11-02 09:00 EST, as the server reports it after expansion.
        let out = update_event(WEEKLY, Some("20261102T140000Z"), &fields, &ny()).unwrap();
        let cal = parse_calendar(&out).unwrap();
        let events: Vec<_> = cal.blocks("VEVENT").collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text("SUMMARY").as_deref(), Some("Weekly sync"));
        let ov = events[1];
        assert_eq!(ov.text("SUMMARY").as_deref(), Some("Moved"));
        assert_eq!(ov.prop("RECURRENCE-ID").unwrap().value, "20261102T090000");
        assert_eq!(ov.prop("DTEND").unwrap().value, "20261102T093000");
        assert!(ov.prop("RRULE").is_none());

        // Editing the same occurrence again updates the override instead of adding another.
        let again = update_event(&out, Some("20261102T140000Z"), &fields, &ny()).unwrap();
        assert_eq!(parse_calendar(&again).unwrap().blocks("VEVENT").count(), 2);
    }

    #[test]
    fn cancel_occurrence_adds_exdate_and_drops_override() {
        let fields = EventFields {
            summary: Some("Moved".into()),
            ..Default::default()
        };
        let with_override = update_event(WEEKLY, Some("20261102T140000Z"), &fields, &ny()).unwrap();

        let out = cancel_occurrence(&with_override, "20261102T140000Z", &ny())
            .unwrap()
            .unwrap();
        let cal = parse_calendar(&out).unwrap();
        assert_eq!(cal.blocks("VEVENT").count(), 1);
        assert!(out.contains("EXDATE;TZID=America/New_York:20261102T090000\r\n"));

        // A second cancellation adds another EXDATE rather than replacing the first.
        let out = cancel_occurrence(&out, "20261109T140000Z", &ny())
            .unwrap()
            .unwrap();
        assert_eq!(out.matches("EXDATE").count(), 2);
    }

    #[test]
    fn cancel_only_override_empties_resource() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nRECURRENCE-ID:20261102T140000Z\r\n\
DTSTART:20261102T140000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(
            cancel_occurrence(ics, "20261102T140000Z", &ny())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn busy_follows_transp_and_status() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261102T140000Z\r\nDTEND:20261102T150000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:b\r\nDTSTART:20261102T140000Z\r\nTRANSP:TRANSPARENT\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:c\r\nDTSTART:20261102T140000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:d\r\nDTSTART:20261102\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20261102\r\nTRANSP:OPAQUE\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
        let busy: Vec<_> = parse_events("/c/a.ics", "e", ics, &ny())
            .unwrap()
            .iter()
            .map(|e| e.busy)
            .collect();
        assert_eq!(busy, [true, false, false, false, true]);
    }

    #[test]
    fn new_event_is_readable() {
        let fields = EventFields {
            summary: Some("Dentist".into()),
            start: Some(Time::parse_input("2026-12-01T15:00", &ny()).unwrap()),
            end: Some(Time::parse_input("2026-12-01T16:00", &ny()).unwrap()),
            location: Some("123 Main St".into()),
            ..Default::default()
        };
        let ics = new_event("abc", &fields, &ny()).unwrap();
        assert!(ics.contains("DTSTART:20261201T200000Z\r\n"));
        let ev = &parse_events("/c/abc.ics", "e", &ics, &ny()).unwrap()[0];
        assert_eq!(ev.location.as_deref(), Some("123 Main St"));
    }

    #[test]
    fn rejects_end_before_start() {
        let fields = EventFields {
            summary: Some("x".into()),
            start: Some(Time::parse_input("2026-12-02", &ny()).unwrap()),
            end: Some(Time::parse_input("2026-12-01", &ny()).unwrap()),
            ..Default::default()
        };
        assert!(matches!(
            new_event("x", &fields, &ny()),
            Err(IcalError::EndBeforeStart)
        ));
    }
}

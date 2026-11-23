//! Finding free time between events.

use jiff::civil::Time as CivilTime;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ZonedDateTime};

use crate::ical::Event;

#[derive(Debug, serde::Serialize)]
pub struct Slot {
    pub start: ZonedDateTime,
    pub end: ZonedDateTime,
    pub minutes: i64,
}

/// Local hours of the day to search within, e.g. 09:00-17:00.
pub struct DailyHours {
    pub start: CivilTime,
    pub end: CivilTime,
}

/// Free slots of at least `min_len` within `[start, end)`, around the busy
/// `events`, optionally limited to `hours` on each day (local to `tz`).
pub fn free_slots(
    events: &[Event],
    start: Timestamp,
    end: Timestamp,
    min_len: SignedDuration,
    hours: Option<&DailyHours>,
    tz: &TimeZone,
) -> Result<Vec<Slot>, jiff::Error> {
    let mut busy = events
        .iter()
        .filter(|e| e.busy)
        .map(|e| e.interval(tz))
        .collect::<Result<Vec<_>, _>>()?;
    busy.sort();

    let mut slots = Vec::new();
    for (window_start, window_end) in windows(start, end, hours, tz)? {
        let mut cursor = window_start;
        for &(busy_start, busy_end) in &busy {
            if busy_start >= window_end {
                break;
            }
            if busy_start > cursor {
                push_slot(&mut slots, cursor, busy_start, min_len, tz);
            }
            cursor = cursor.max(busy_end);
        }
        if cursor < window_end {
            push_slot(&mut slots, cursor, window_end, min_len, tz);
        }
    }
    Ok(slots)
}

/// The parts of `[start, end)` to search: all of it, or `hours` on each day.
fn windows(
    start: Timestamp,
    end: Timestamp,
    hours: Option<&DailyHours>,
    tz: &TimeZone,
) -> Result<Vec<(Timestamp, Timestamp)>, jiff::Error> {
    let Some(hours) = hours else {
        return Ok(vec![(start, end)]);
    };

    let mut windows = Vec::new();
    let mut day = start.to_zoned_datetime(tz.clone()).date();
    let last = end.to_zoned_datetime(tz.clone()).date();
    while day <= last {
        let open = day.to_datetime(hours.start).to_zoned_datetime(tz.clone())?.timestamp();
        let close = day.to_datetime(hours.end).to_zoned_datetime(tz.clone())?.timestamp();
        let (open, close) = (open.max(start), close.min(end));
        if open < close {
            windows.push((open, close));
        }
        day = day.tomorrow()?;
    }
    Ok(windows)
}

fn push_slot(slots: &mut Vec<Slot>, start: Timestamp, end: Timestamp, min_len: SignedDuration, tz: &TimeZone) {
    let len = end.duration_since(start);
    if len >= min_len {
        slots.push(Slot {
            start: start.to_zoned_datetime(tz.clone()),
            end: end.to_zoned_datetime(tz.clone()),
            minutes: len.as_mins(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny() -> TimeZone {
        TimeZone::get("America/New_York").unwrap()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse::<jiff::civil::DateTime>().unwrap().to_zoned_datetime(ny()).unwrap().timestamp()
    }

    use crate::ical::EventTime;

    fn event(start: &str, end: &str, busy: bool) -> Event {
        Event {
            id: String::new(),
            etag: String::new(),
            uid: String::new(),
            summary: String::new(),
            time: EventTime::Timed {
                start: ts(start).to_zoned_datetime(ny()),
                end: ts(end).to_zoned_datetime(ny()),
            },
            location: None,
            description: None,
            recurring: false,
            recurrence_id: None,
            busy,
        }
    }

    fn spans(slots: &[Slot]) -> Vec<String> {
        slots
            .iter()
            .map(|s| format!("{}-{}", s.start.strftime("%d %H:%M"), s.end.strftime("%d %H:%M")))
            .collect()
    }

    #[test]
    fn gaps_between_overlapping_and_free_events() {
        let events = [
            event("2026-12-01T10:00", "2026-12-01T11:00", true),
            event("2026-12-01T10:30", "2026-12-01T12:00", true), // overlaps the first
            event("2026-12-01T13:00", "2026-12-01T14:00", false), // not busy
            event("2026-12-01T15:00", "2026-12-01T15:20", true),
        ];
        let slots = free_slots(
            &events,
            ts("2026-12-01T09:00"),
            ts("2026-12-01T17:00"),
            SignedDuration::from_mins(30),
            None,
            &ny(),
        )
        .unwrap();
        assert_eq!(spans(&slots), ["01 09:00-01 10:00", "01 12:00-01 15:00", "01 15:20-01 17:00"]);
    }

    #[test]
    fn daily_hours_split_by_day_and_drop_short_slots() {
        let events = [event("2026-12-01T09:00", "2026-12-01T16:45", true)];
        let hours = DailyHours { start: "09:00".parse().unwrap(), end: "17:00".parse().unwrap() };
        let slots = free_slots(
            &events,
            ts("2026-12-01T00:00"),
            ts("2026-12-03T00:00"),
            SignedDuration::from_mins(30),
            Some(&hours),
            &ny(),
        )
        .unwrap();
        // Dec 1 only has 15 minutes left; Dec 3 is outside the range.
        assert_eq!(spans(&slots), ["02 09:00-02 17:00"]);
    }
}

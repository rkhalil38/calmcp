use std::sync::Arc;

use rmcp::{ServerHandler, handler::server::wrapper::Parameters, schemars, serde_json, tool, tool_handler, tool_router};

use jiff::tz::TimeZone;

use crate::dav::{CalDav, create_event, delete_event, list_calendars, list_events, update_event};
use crate::free::{DailyHours, free_slots};
use crate::ical::{EventFields, Time};

#[derive(serde::Serialize)]
struct CalendarSummary {
    id: String,
    name: String
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ListEventsParams {
    /// Calendar id, as returned by list_calendars.
    calendar_id: String,
    /// Start of the time frame (inclusive), as an RFC 3339 timestamp, e.g. "2026-11-23T00:00:00Z".
    start: String,
    /// End of the time frame (exclusive), as an RFC 3339 timestamp.
    end: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct CreateEventParams {
    /// Calendar id, as returned by list_calendars.
    calendar_id: String,
    summary: String,
    /// "YYYY-MM-DD" for an all-day event, "YYYY-MM-DDTHH:MM" for a local time in the user's
    /// timezone, or an RFC 3339 timestamp.
    start: String,
    /// Same format as start. Exclusive: an all-day event on Dec 1 ends "2026-12-02".
    end: String,
    location: Option<String>,
    description: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct UpdateEventParams {
    /// Event id, as returned by list_events.
    id: String,
    /// Event etag, as returned by list_events or a previous create/update.
    etag: String,
    /// To edit a single occurrence of a recurring event, its recurrence_id from list_events.
    /// Omit to edit the event itself, which for a recurring event means the whole series.
    recurrence_id: Option<String>,
    summary: Option<String>,
    /// Same formats as create_event. If only start is given, the event keeps its length.
    start: Option<String>,
    end: Option<String>,
    /// An empty string removes the location.
    location: Option<String>,
    /// An empty string removes the description.
    description: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct DeleteEventParams {
    /// Event id, as returned by list_events.
    id: String,
    /// Event etag, as returned by list_events or a previous create/update.
    etag: String,
    /// To delete a single occurrence of a recurring event, its recurrence_id from list_events.
    /// Omit to delete the event itself, which for a recurring event means the whole series.
    recurrence_id: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct FindFreeTimeParams {
    /// Start of the search, in the same formats as create_event. A date means the start of that day.
    start: String,
    /// End of the search (exclusive), in the same formats. A date means the start of that day.
    end: String,
    /// Minimum length of a free slot, in minutes. Defaults to 30.
    duration_minutes: Option<u32>,
    /// Calendars to check, as returned by list_calendars. Defaults to all of them.
    calendar_ids: Option<Vec<String>>,
    /// Only search between these local times each day, as "HH:MM", e.g. "09:00". Requires day_end.
    day_start: Option<String>,
    /// End of the daily search hours, as "HH:MM", e.g. "17:00". Requires day_start.
    day_end: Option<String>,
}

#[derive(Clone)]
pub struct CalMcp {
    client: Arc<CalDav>,
    tz: TimeZone,
}

impl CalMcp {
    fn parse_time(&self, field: &str, value: Option<&str>) -> Result<Option<Time>, String> {
        value
            .map(|v| Time::parse_input(v, &self.tz).map_err(|e| format!("Invalid {field}: {e}")))
            .transpose()
    }
}

#[tool_router]
impl CalMcp {
    pub fn new(client: Arc<CalDav>, tz: TimeZone) -> Self {
        Self { client, tz }
    }

    #[tool(description = "List the user's calendars.")]
    async fn list_calendars(&self) -> Result<String, String> {
        let calendars = list_calendars(&self.client)
            .await
            .map_err(|e| format!("Could not list calendars: {e}"))?;

        let summaries: Vec<CalendarSummary> = calendars
            .into_iter()
            .map(|c| {
                let name = c.name.unwrap_or_else(|| c.href.clone());
                CalendarSummary { id: c.href, name }
            })
        .collect();

        serde_json::to_string(&summaries).map_err(|e| format!("Could not serialize calendars: {e}"))
    }

    #[tool(description = "List the user's events within a specified time frame.")]
    async fn list_events(&self, Parameters(params): Parameters<ListEventsParams>) -> Result<String, String> {
        let start: jiff::Timestamp = params.start.parse().map_err(|e| format!("Invalid start: {e}"))?;
        let end: jiff::Timestamp = params.end.parse().map_err(|e| format!("Invalid end: {e}"))?;

        let events = list_events(&self.client, &params.calendar_id, &self.tz, start, end)
            .await
            .map_err(|e| format!("Could not list events: {e}"))?;

        serde_json::to_string(&events).map_err(|e| format!("Could not serialize events: {e}"))
    }

    #[tool(description = "Create an event. Returns its id and etag.")]
    async fn create_event(&self, Parameters(params): Parameters<CreateEventParams>) -> Result<String, String> {
        let fields = EventFields {
            summary: Some(params.summary),
            start: self.parse_time("start", Some(&params.start))?,
            end: self.parse_time("end", Some(&params.end))?,
            location: params.location,
            description: params.description,
        };

        let saved = create_event(&self.client, &params.calendar_id, &fields, &self.tz)
            .await
            .map_err(|e| format!("Could not create event: {e}"))?;

        serde_json::to_string(&saved).map_err(|e| format!("Could not serialize result: {e}"))
    }

    #[tool(description = "Delete an event, or a single occurrence of a recurring event. \
        Returns {\"deleted\": id} when the event is gone, or the remaining event's id and new etag \
        when only an occurrence was removed.")]
    async fn delete_event(&self, Parameters(params): Parameters<DeleteEventParams>) -> Result<String, String> {
        let remaining = delete_event(
            &self.client,
            &params.id,
            &params.etag,
            params.recurrence_id.as_deref(),
            &self.tz,
        )
        .await
        .map_err(|e| format!("Could not delete event: {e}"))?;

        let result = match remaining {
            Some(saved) => serde_json::to_value(&saved),
            None => Ok(serde_json::json!({ "deleted": params.id })),
        };
        result
            .map(|v| v.to_string())
            .map_err(|e| format!("Could not serialize result: {e}"))
    }

    #[tool(description = "Find free time slots across the user's calendars. Timed events block \
        time unless marked free; all-day events only block time when marked busy.")]
    async fn find_free_time(&self, Parameters(params): Parameters<FindFreeTimeParams>) -> Result<String, String> {
        let instant = |field: &str, value: &str| {
            Time::parse_input(value, &self.tz)
                .and_then(|t| t.timestamp(&self.tz))
                .map_err(|e| format!("Invalid {field}: {e}"))
        };
        let (start, end) = (instant("start", &params.start)?, instant("end", &params.end)?);
        if end <= start {
            return Err("end must be after start".into());
        }

        let hours = match (params.day_start, params.day_end) {
            (None, None) => None,
            (Some(s), Some(e)) => {
                let start = s.parse().map_err(|e| format!("Invalid day_start: {e}"))?;
                let end = e.parse().map_err(|e| format!("Invalid day_end: {e}"))?;
                if end <= start {
                    return Err("day_end must be after day_start".into());
                }
                Some(DailyHours { start, end })
            }
            _ => return Err("day_start and day_end must be given together".into()),
        };

        let calendar_ids = match params.calendar_ids {
            Some(ids) => ids,
            None => list_calendars(&self.client)
                .await
                .map_err(|e| format!("Could not list calendars: {e}"))?
                .into_iter()
                .map(|c| c.href)
                .collect(),
        };

        let mut events = Vec::new();
        for id in &calendar_ids {
            let mut found = list_events(&self.client, id, &self.tz, start, end)
                .await
                .map_err(|e| format!("Could not list events in {id}: {e}"))?;
            events.append(&mut found);
        }

        let min_len = jiff::SignedDuration::from_mins(params.duration_minutes.unwrap_or(30).into());
        let slots = free_slots(&events, start, end, min_len, hours.as_ref(), &self.tz)
            .map_err(|e| format!("Could not compute free time: {e}"))?;

        serde_json::to_string(&slots).map_err(|e| format!("Could not serialize free time: {e}"))
    }

    #[tool(description = "Update an event. Only the given fields change. Returns its id and new etag \
        (null if the server didn't return one; list events again to get it).")]
    async fn update_event(&self, Parameters(params): Parameters<UpdateEventParams>) -> Result<String, String> {
        let fields = EventFields {
            summary: params.summary,
            start: self.parse_time("start", params.start.as_deref())?,
            end: self.parse_time("end", params.end.as_deref())?,
            location: params.location,
            description: params.description,
        };

        let saved = update_event(
            &self.client,
            &params.id,
            &params.etag,
            params.recurrence_id.as_deref(),
            &fields,
            &self.tz,
        )
        .await
        .map_err(|e| format!("Could not update event: {e}"))?;

        serde_json::to_string(&saved).map_err(|e| format!("Could not serialize result: {e}"))
    }
}

#[tool_handler(name = "CalMcp", version = "0.0.1", instructions = "An interface for managing a user's CalDAV server.")]
impl ServerHandler for CalMcp {}

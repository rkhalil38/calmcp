use std::sync::Arc;

use rmcp::{ServerHandler, serde_json, tool, tool_handler, tool_router};

use crate::dav::{CalDav, ListError, list_calendars};

#[derive(serde::Serialize)]
struct CalendarSummary {
    id: String,
    name: String
}

#[derive(Clone)]
pub struct CalMcp {
    client: Arc<CalDav>
}

#[tool_router]
impl CalMcp {
    pub fn new(client: Arc<CalDav>) -> Self {
        Self { client }
    }

    #[tool(description = "List the user's calendars.")]
    async fn list_calendars(&self) -> Result<String, String> {
        let calendars = list_calendars(&self.client)
            .await
            .map_err(|e| format!("Could not list calendars: {e}"))?;

        let summaries: Vec<CalendarSummary> = calendars
            .into_iter()
            .map(|c| {
                let id = c.href.trim_end_matches('/').rsplit('/').next().unwrap_or(&c.href).to_string();
                let name = c.name.unwrap_or_else(|| id.clone());
                CalendarSummary { id, name }
            })
        .collect();

        serde_json::to_string(&summaries).map_err(|e| format!("Could not serialize calendars: {e}"))
    }
}

#[tool_handler(name = "CalMcp", version = "0.0.1", instructions = "An interface for managing a user's CalDAV server.")]
impl ServerHandler for CalMcp {}

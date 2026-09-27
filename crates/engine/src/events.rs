use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    Created,
    Updated,
    Deleted,
    Inbound,
    Cron,
    /// Inbound email (Cloudflare Email Routing `email()` handler).
    Email,
}

impl EventKind {
    pub fn name(&self) -> &'static str {
        match self {
            EventKind::Created => "record.created",
            EventKind::Updated => "record.updated",
            EventKind::Deleted => "record.deleted",
            EventKind::Inbound => "record.inbound",
            EventKind::Cron => "record.cron",
            EventKind::Email => "email.received",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub board: String,
    pub r#type: String,
    pub seq: Option<i64>,
    pub payload: Json,
}

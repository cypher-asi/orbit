use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a row in the `audit_events` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub event_type: String,
    pub repo_id: Option<Uuid>,
    pub target_id: Option<Uuid>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// Input for creating a new audit event.
///
/// All fields except `event_type` are optional so that system-level events
/// (no actor) or events without a specific repo/target can be recorded.
#[derive(Debug, Clone)]
pub struct NewAuditEvent {
    /// The user who performed the action, if any.
    pub actor_id: Option<Uuid>,
    /// A dotted event type string, e.g. `"repo.created"`.
    pub event_type: String,
    /// The repository this event relates to, if any.
    pub repo_id: Option<Uuid>,
    /// An optional secondary target (e.g. the user being added as collaborator).
    pub target_id: Option<Uuid>,
    /// Arbitrary JSON metadata to attach to the event.
    pub metadata: Option<serde_json::Value>,
}

/// Filter criteria for querying audit events.
///
/// All filter fields are optional; when `None` they are not applied.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Only events by this actor.
    pub actor_id: Option<Uuid>,
    /// Only events for this repository.
    pub repo_id: Option<Uuid>,
    /// Only events of this type.
    pub event_type: Option<String>,
    /// Only events created at or after this timestamp.
    pub since: Option<DateTime<Utc>>,
    /// Only events created at or before this timestamp.
    pub until: Option<DateTime<Utc>>,
    /// Maximum number of results to return.
    pub limit: u32,
    /// Number of results to skip (for pagination).
    pub offset: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_audit_event_minimal() {
        let event = NewAuditEvent {
            actor_id: None,
            event_type: "test.event".to_string(),
            repo_id: None,
            target_id: None,
            metadata: None,
        };
        assert_eq!(event.event_type, "test.event");
        assert!(event.actor_id.is_none());
    }

    #[test]
    fn new_audit_event_full() {
        let actor = Uuid::new_v4();
        let repo = Uuid::new_v4();
        let target = Uuid::new_v4();
        let event = NewAuditEvent {
            actor_id: Some(actor),
            event_type: "repo.created".to_string(),
            repo_id: Some(repo),
            target_id: Some(target),
            metadata: Some(serde_json::json!({"name": "test"})),
        };
        assert_eq!(event.actor_id, Some(actor));
        assert_eq!(event.repo_id, Some(repo));
        assert_eq!(event.target_id, Some(target));
        assert!(event.metadata.is_some());
    }

    #[test]
    fn event_filter_default() {
        let filter = EventFilter::default();
        assert!(filter.actor_id.is_none());
        assert!(filter.repo_id.is_none());
        assert!(filter.event_type.is_none());
        assert!(filter.since.is_none());
        assert!(filter.until.is_none());
        assert_eq!(filter.limit, 0);
        assert_eq!(filter.offset, 0);
    }

    #[test]
    fn audit_event_serializes() {
        let event = AuditEvent {
            id: Uuid::nil(),
            actor_id: Some(Uuid::nil()),
            event_type: "repo.created".to_string(),
            repo_id: None,
            target_id: None,
            metadata: Some(serde_json::json!({"key": "value"})),
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["eventType"], "repo.created");
        assert_eq!(json["metadata"]["key"], "value");
    }
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a row in the `jobs` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Job {
    pub id: Uuid,
    pub job_type: String,
    pub payload: serde_json::Value,
    /// Stored as a VARCHAR in Postgres (e.g. "pending", "running", "completed", "failed").
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub last_error: Option<String>,
    pub run_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_round_trip_serialization() {
        let job = Job {
            id: Uuid::nil(),
            job_type: "test".to_string(),
            payload: serde_json::json!({}),
            status: "pending".to_string(),
            attempts: 0,
            max_attempts: 3,
            last_error: None,
            run_at: Utc::now(),
            completed_at: None,
            created_at: Utc::now(),
        };
        let json = serde_json::to_value(&job).unwrap();
        let _: Job = serde_json::from_value(json).unwrap();
    }
}

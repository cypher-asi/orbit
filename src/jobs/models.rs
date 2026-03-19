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

impl Job {
    /// Parse the `status` string column into a typed `JobStatus`.
    pub fn job_status(&self) -> JobStatus {
        JobStatus::try_from(self.status.as_str()).unwrap_or(JobStatus::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_status_as_str() {
        assert_eq!(JobStatus::Pending.as_str(), "pending");
        assert_eq!(JobStatus::Running.as_str(), "running");
        assert_eq!(JobStatus::Completed.as_str(), "completed");
        assert_eq!(JobStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn job_status_display() {
        assert_eq!(format!("{}", JobStatus::Pending), "pending");
        assert_eq!(format!("{}", JobStatus::Failed), "failed");
    }

    #[test]
    fn job_status_try_from_valid() {
        assert_eq!(JobStatus::try_from("pending"), Ok(JobStatus::Pending));
        assert_eq!(JobStatus::try_from("running"), Ok(JobStatus::Running));
        assert_eq!(JobStatus::try_from("completed"), Ok(JobStatus::Completed));
        assert_eq!(JobStatus::try_from("failed"), Ok(JobStatus::Failed));
    }

    #[test]
    fn job_status_try_from_invalid() {
        let result = JobStatus::try_from("bogus");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "unknown job status: bogus");
    }

    #[test]
    fn job_status_serializes_lowercase() {
        let json = serde_json::to_value(JobStatus::Running).unwrap();
        assert_eq!(json, serde_json::json!("running"));
    }

    #[test]
    fn job_status_deserializes_lowercase() {
        let status: JobStatus = serde_json::from_str(r#""completed""#).unwrap();
        assert_eq!(status, JobStatus::Completed);
    }

    #[test]
    fn job_job_status_helper() {
        let job = Job {
            id: Uuid::nil(),
            job_type: "test".to_string(),
            payload: serde_json::json!({}),
            status: "failed".to_string(),
            attempts: 2,
            max_attempts: 3,
            last_error: Some("oops".to_string()),
            run_at: Utc::now(),
            completed_at: None,
            created_at: Utc::now(),
        };
        assert_eq!(job.job_status(), JobStatus::Failed);
    }
}

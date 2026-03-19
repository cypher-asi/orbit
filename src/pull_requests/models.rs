use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// PrStatus enum
// ---------------------------------------------------------------------------

/// Status of a pull request.
///
/// Stored in the `pull_requests.status` column as a lowercase string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrStatus {
    Open,
    Closed,
    Merged,
}

impl PrStatus {
    /// Database string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            PrStatus::Open => "open",
            PrStatus::Closed => "closed",
            PrStatus::Merged => "merged",
        }
    }

    /// Parse from a database string value.
    pub fn from_db_str(s: &str) -> Option<PrStatus> {
        match s {
            "open" => Some(PrStatus::Open),
            "closed" => Some(PrStatus::Closed),
            "merged" => Some(PrStatus::Merged),
            _ => None,
        }
    }
}

impl std::fmt::Display for PrStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// sqlx encoding/decoding: store as TEXT matching the DB varchar column.
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for PrStatus {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        PrStatus::from_db_str(s).ok_or_else(|| format!("unknown PR status: {}", s).into())
    }
}

impl sqlx::Type<sqlx::Postgres> for PrStatus {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <&str as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <&str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for PrStatus {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.as_str(), buf)
    }
}

// ---------------------------------------------------------------------------
// PullRequest (database row)
// ---------------------------------------------------------------------------

/// Represents a row in the `pull_requests` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub author_id: Uuid,
    pub number: i32,
    pub source_branch: String,
    pub target_branch: String,
    pub title: String,
    pub description: Option<String>,
    pub status: PrStatus,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// Input for creating a new pull request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePrInput {
    pub repo_id: Uuid,
    pub author_id: Uuid,
    pub source_branch: String,
    pub target_branch: String,
    pub title: String,
    pub description: Option<String>,
}

/// Input for updating an existing pull request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePrInput {
    pub title: Option<String>,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// PrFilter
// ---------------------------------------------------------------------------

/// Filter criteria for listing pull requests.
#[derive(Debug, Clone)]
pub struct PrFilter {
    pub status: Option<PrStatus>,
    pub author_id: Option<Uuid>,
    pub limit: u32,
    pub offset: u32,
}

impl Default for PrFilter {
    fn default() -> Self {
        PrFilter {
            status: None,
            author_id: None,
            limit: 20,
            offset: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// MergeabilityState
// ---------------------------------------------------------------------------

/// The mergeability state of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeabilityState {
    /// Can be merged without conflicts.
    Clean,
    /// Has merge conflicts.
    Conflicting,
    /// Not yet computed.
    Unknown,
    /// Source or target branch missing.
    InvalidRef,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- PrStatus tests -----------------------------------------------------

    #[test]
    fn pr_status_as_str_round_trip() {
        for s in &[PrStatus::Open, PrStatus::Closed, PrStatus::Merged] {
            let str_val = s.as_str();
            let parsed = PrStatus::from_db_str(str_val).unwrap();
            assert_eq!(*s, parsed);
        }
    }

    #[test]
    fn pr_status_from_db_str_unknown() {
        assert!(PrStatus::from_db_str("invalid").is_none());
    }

    #[test]
    fn pr_status_display() {
        assert_eq!(PrStatus::Open.to_string(), "open");
        assert_eq!(PrStatus::Closed.to_string(), "closed");
        assert_eq!(PrStatus::Merged.to_string(), "merged");
    }

    #[test]
    fn pr_status_serde_serialize() {
        let json = serde_json::to_string(&PrStatus::Open).unwrap();
        assert_eq!(json, r#""open""#);
        let json = serde_json::to_string(&PrStatus::Closed).unwrap();
        assert_eq!(json, r#""closed""#);
        let json = serde_json::to_string(&PrStatus::Merged).unwrap();
        assert_eq!(json, r#""merged""#);
    }

    #[test]
    fn pr_status_serde_deserialize() {
        let v: PrStatus = serde_json::from_str(r#""open""#).unwrap();
        assert_eq!(v, PrStatus::Open);
        let v: PrStatus = serde_json::from_str(r#""closed""#).unwrap();
        assert_eq!(v, PrStatus::Closed);
        let v: PrStatus = serde_json::from_str(r#""merged""#).unwrap();
        assert_eq!(v, PrStatus::Merged);
    }

    // -- MergeabilityState tests --------------------------------------------

    #[test]
    fn mergeability_state_serde_round_trip() {
        let states = [
            MergeabilityState::Clean,
            MergeabilityState::Conflicting,
            MergeabilityState::Unknown,
            MergeabilityState::InvalidRef,
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let parsed: MergeabilityState = serde_json::from_str(&json).unwrap();
            assert_eq!(*state, parsed);
        }
    }

    #[test]
    fn mergeability_state_serializes_snake_case() {
        let json = serde_json::to_string(&MergeabilityState::InvalidRef).unwrap();
        assert_eq!(json, r#""invalid_ref""#);
        let json = serde_json::to_string(&MergeabilityState::Clean).unwrap();
        assert_eq!(json, r#""clean""#);
    }

    // -- CreatePrInput tests ------------------------------------------------

    #[test]
    fn create_pr_input_deserialize_full() {
        let json = r#"{
            "repoId": "00000000-0000-0000-0000-000000000001",
            "authorId": "00000000-0000-0000-0000-000000000002",
            "sourceBranch": "feature/foo",
            "targetBranch": "main",
            "title": "Add feature foo",
            "description": "This adds the foo feature"
        }"#;
        let input: CreatePrInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.source_branch, "feature/foo");
        assert_eq!(input.target_branch, "main");
        assert_eq!(input.title, "Add feature foo");
        assert_eq!(
            input.description.as_deref(),
            Some("This adds the foo feature")
        );
    }

    #[test]
    fn create_pr_input_deserialize_no_description() {
        let json = r#"{
            "repoId": "00000000-0000-0000-0000-000000000001",
            "authorId": "00000000-0000-0000-0000-000000000002",
            "sourceBranch": "feature/bar",
            "targetBranch": "main",
            "title": "Add feature bar"
        }"#;
        let input: CreatePrInput = serde_json::from_str(json).unwrap();
        assert!(input.description.is_none());
    }

    // -- UpdatePrInput tests ------------------------------------------------

    #[test]
    fn update_pr_input_deserialize_partial() {
        let json = r#"{"title": "New title"}"#;
        let input: UpdatePrInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.title.as_deref(), Some("New title"));
        assert!(input.description.is_none());
    }

    #[test]
    fn update_pr_input_deserialize_empty() {
        let json = r#"{}"#;
        let input: UpdatePrInput = serde_json::from_str(json).unwrap();
        assert!(input.title.is_none());
        assert!(input.description.is_none());
    }

    // -- PrFilter tests -----------------------------------------------------

    #[test]
    fn pr_filter_default() {
        let filter = PrFilter::default();
        assert!(filter.status.is_none());
        assert!(filter.author_id.is_none());
        assert_eq!(filter.limit, 20);
        assert_eq!(filter.offset, 0);
    }
}

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::AdminUser;
use crate::auth::middleware::RequireAuth;
use crate::errors::ApiError;
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::service as repo_service;
use crate::users::service as user_service;

use super::models::{AuditEvent, EventFilter};
use super::service;

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Query parameters for `GET /admin/events`.
#[derive(Debug, Deserialize)]
pub struct AdminEventsQuery {
    pub actor_id: Option<Uuid>,
    pub repo_id: Option<Uuid>,
    pub event_type: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Query parameters for `GET /repos/{owner}/{repo}/events`.
#[derive(Debug, Deserialize)]
pub struct RepoEventsQuery {
    pub event_type: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `{owner}/{repo}` routes.
#[derive(Debug, Deserialize)]
pub struct OwnerRepoPath {
    pub owner: String,
    pub repo: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /admin/events -- List/filter audit events (admin only).
///
/// Accepts optional query parameters: actor_id, repo_id, event_type, since,
/// until, limit, offset. Returns a paginated list of audit events ordered by
/// `created_at DESC`.
async fn admin_list_events(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(params): Query<AdminEventsQuery>,
) -> Result<Json<Vec<AuditEvent>>, ApiError> {
    let filter = EventFilter {
        actor_id: params.actor_id,
        repo_id: params.repo_id,
        event_type: params.event_type,
        since: params.since,
        until: params.until,
        limit: params.limit.unwrap_or(50).min(200).max(1),
        offset: params.offset.unwrap_or(0),
    };

    let events = service::list_events(&state.db, filter).await?;
    Ok(Json(events))
}

/// GET /admin/events/{id} -- Get a single audit event by ID (admin only).
async fn admin_get_event(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AuditEvent>, ApiError> {
    let event = service::get_event(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("event not found".to_string()))?;

    Ok(Json(event))
}

/// GET /repos/{owner}/{repo}/events -- List audit events scoped to a
/// repository (owner/admin access required).
///
/// Resolves the repository from the `{owner}/{repo}` path, checks that the
/// authenticated user has Admin (owner) permission on the repo, then returns
/// events filtered by `repo_id`.
async fn repo_events(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
    Query(params): Query<RepoEventsQuery>,
) -> Result<Json<Vec<AuditEvent>>, ApiError> {
    // Resolve owner and repo.
    let owner = user_service::get_user_by_username(&state.db, &path.owner)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    let repo = repo_service::get_repo_by_owner_and_slug(&state.db, owner.id, &path.repo)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    // Require Admin (owner) permission on the repo.
    permissions_service::check_repo_access(
        &state.db,
        Some(user.id),
        repo.id,
        Permission::Admin,
    )
    .await?;

    let filter = EventFilter {
        actor_id: None,
        repo_id: Some(repo.id),
        event_type: params.event_type,
        since: params.since,
        until: params.until,
        limit: params.limit.unwrap_or(50).min(200).max(1),
        offset: params.offset.unwrap_or(0),
    };

    let events = service::list_events(&state.db, filter).await?;
    Ok(Json(events))
}

// ---------------------------------------------------------------------------
// Router builders
// ---------------------------------------------------------------------------

/// Build the Router for admin audit event endpoints.
///
/// Mounts:
/// - `GET /admin/events`      -- list/filter audit events
/// - `GET /admin/events/{id}` -- get single event
pub fn admin_event_routes() -> Router<AppState> {
    Router::new()
        .route("/admin/events", get(admin_list_events))
        .route("/admin/events/{id}", get(admin_get_event))
}

/// Build the Router for repo-scoped audit event endpoints.
///
/// Mounts:
/// - `GET /repos/{owner}/{repo}/events` -- repo-scoped events
pub fn repo_event_routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/events", get(repo_events))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_events_query_defaults() {
        let query: AdminEventsQuery = serde_json::from_str("{}").unwrap();
        assert!(query.actor_id.is_none());
        assert!(query.repo_id.is_none());
        assert!(query.event_type.is_none());
        assert!(query.since.is_none());
        assert!(query.until.is_none());
        assert!(query.limit.is_none());
        assert!(query.offset.is_none());
    }

    #[test]
    fn admin_events_query_with_values() {
        let json = r#"{
            "actor_id": "00000000-0000-0000-0000-000000000001",
            "repo_id": "00000000-0000-0000-0000-000000000002",
            "event_type": "repo.created",
            "limit": 10,
            "offset": 20
        }"#;
        let query: AdminEventsQuery = serde_json::from_str(json).unwrap();
        assert!(query.actor_id.is_some());
        assert!(query.repo_id.is_some());
        assert_eq!(query.event_type.as_deref(), Some("repo.created"));
        assert_eq!(query.limit, Some(10));
        assert_eq!(query.offset, Some(20));
    }

    #[test]
    fn repo_events_query_defaults() {
        let query: RepoEventsQuery = serde_json::from_str("{}").unwrap();
        assert!(query.event_type.is_none());
        assert!(query.since.is_none());
        assert!(query.until.is_none());
        assert!(query.limit.is_none());
        assert!(query.offset.is_none());
    }

    #[test]
    fn limit_clamping_logic() {
        // Verify the clamping expression produces expected values.
        let clamp = |limit: Option<u32>| limit.unwrap_or(50).min(200).max(1);
        assert_eq!(clamp(None), 50);
        assert_eq!(clamp(Some(500)), 200);
        assert_eq!(clamp(Some(0)), 1);
        assert_eq!(clamp(Some(10)), 10);
    }

    #[test]
    fn owner_repo_path_deserializes() {
        let json = r#"{"owner": "alice", "repo": "my-repo"}"#;
        let path: OwnerRepoPath = serde_json::from_str(json).unwrap();
        assert_eq!(path.owner, "alice");
        assert_eq!(path.repo, "my-repo");
    }
}

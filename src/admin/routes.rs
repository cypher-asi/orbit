use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::middleware::InternalAuth;
use crate::errors::ApiError;
use crate::events;
use crate::events::models::NewAuditEvent;
use crate::jobs;
use crate::jobs::models::Job;
use crate::repos::models::RepoResponse;
use crate::repos::service as repo_service;
use crate::storage::service as storage_service;

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Query parameters for GET /admin/repos.
#[derive(Debug, Deserialize)]
pub struct AdminListReposQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Optional name prefix search.
    pub search: Option<String>,
}

/// Query parameters for GET /admin/jobs.
#[derive(Debug, Deserialize)]
pub struct AdminListJobsQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Optional status filter (pending, running, completed, failed).
    pub status: Option<String>,
}

/// Response for repo details with storage status.
#[derive(Debug, Serialize)]
pub struct AdminRepoDetailResponse {
    #[serde(flatten)]
    pub repo: RepoResponse,
    pub storage_exists: bool,
}

// ---------------------------------------------------------------------------
// Repo management handlers
// ---------------------------------------------------------------------------

/// GET /admin/repos - List all repos with pagination and optional search.
async fn list_repos(
    _admin: InternalAuth,
    State(state): State<AppState>,
    Query(params): Query<AdminListReposQuery>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 100);
    let offset = params.offset.unwrap_or(0).max(0);

    let repos =
        repo_service::list_all_repos(&state.db, limit, offset, params.search.as_deref()).await?;

    let response: Vec<RepoResponse> = repos.into_iter().map(RepoResponse::from).collect();
    Ok(Json(response))
}

/// GET /admin/repos/{id} - Get repo details + storage status.
async fn get_repo(
    _admin: InternalAuth,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminRepoDetailResponse>, ApiError> {
    let repo = repo_service::get_repo(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    let storage_config = storage_service::StorageConfig::new(state.git_storage_root.clone());
    let storage_exists = storage_service::repo_exists(&storage_config, id).await;

    Ok(Json(AdminRepoDetailResponse {
        repo: RepoResponse::from(repo),
        storage_exists,
    }))
}

/// POST /admin/repos/{id}/archive - Archive a repository.
async fn archive_repo(
    _admin: InternalAuth,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    repo_service::archive_repo(&state.db, id, Uuid::nil()).await?;

    // Emit audit event
    events::emit(
        &state.db,
        NewAuditEvent {
            actor_id: None,
            event_type: "admin.repo_archived".to_string(),
            repo_id: Some(id),
            target_id: None,
            metadata: None,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Job management handlers
// ---------------------------------------------------------------------------

/// GET /admin/jobs - List jobs with optional status filter.
async fn list_jobs(
    _admin: InternalAuth,
    State(state): State<AppState>,
    Query(params): Query<AdminListJobsQuery>,
) -> Result<Json<Vec<Job>>, ApiError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 100);
    let offset = params.offset.unwrap_or(0).max(0);

    let jobs_list = jobs::list_jobs(&state.db, limit, offset, params.status.as_deref()).await?;

    Ok(Json(jobs_list))
}

/// GET /admin/jobs/failed - List failed jobs.
async fn list_failed_jobs(
    _admin: InternalAuth,
    State(state): State<AppState>,
    Query(params): Query<AdminListJobsQuery>,
) -> Result<Json<Vec<Job>>, ApiError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 100) as u32;

    let jobs_list = jobs::list_failed(&state.db, limit).await?;
    Ok(Json(jobs_list))
}

/// POST /admin/jobs/{id}/retry - Retry a failed job.
async fn retry_job(
    _admin: InternalAuth,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    jobs::retry(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build a Router for admin mutation endpoints only.
///
/// These are state-changing operations that should be rate-limited for
/// defense-in-depth, even though they already require admin authentication.
///
/// Mounts:
/// - `POST   /admin/users/{id}/disable` -- disable user
/// - `POST   /admin/users/{id}/enable`  -- enable user
/// - `POST   /admin/repos/{id}/archive` -- archive repo
/// - `POST   /admin/jobs/{id}/retry`    -- retry failed job
pub fn admin_mutation_routes() -> Router<AppState> {
    Router::new()
        .route("/admin/repos/{id}/archive", post(archive_repo))
        .route("/admin/jobs/{id}/retry", post(retry_job))
}

/// Build a Router for admin read-only endpoints.
///
/// These are GET operations that don't require rate limiting beyond the
/// global middleware stack.
///
/// Mounts:
/// - `GET    /admin/users`              -- list users (pagination + search)
/// - `GET    /admin/users/{id}`         -- get user details
/// - `GET    /admin/repos`              -- list repos (pagination + search)
/// - `GET    /admin/repos/{id}`         -- get repo details + storage status
/// - `GET    /admin/jobs`               -- list jobs (status filter)
/// - `GET    /admin/jobs/failed`        -- list failed jobs
pub fn admin_read_routes() -> Router<AppState> {
    Router::new()
        .route("/admin/repos", get(list_repos))
        .route("/admin/repos/{id}", get(get_repo))
        .route("/admin/jobs", get(list_jobs))
        .route("/admin/jobs/failed", get(list_failed_jobs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_list_repos_query_defaults() {
        let query: AdminListReposQuery = serde_json::from_str("{}").unwrap();
        assert!(query.limit.is_none());
        assert!(query.offset.is_none());
        assert!(query.search.is_none());
    }

    #[test]
    fn admin_list_repos_query_with_values() {
        let query: AdminListReposQuery =
            serde_json::from_str(r#"{"limit": 25, "offset": 0, "search": "orbit"}"#).unwrap();
        assert_eq!(query.limit, Some(25));
        assert_eq!(query.offset, Some(0));
        assert_eq!(query.search.as_deref(), Some("orbit"));
    }

    #[test]
    fn admin_list_jobs_query_defaults() {
        let query: AdminListJobsQuery = serde_json::from_str("{}").unwrap();
        assert!(query.limit.is_none());
        assert!(query.offset.is_none());
        assert!(query.status.is_none());
    }

    #[test]
    fn admin_list_jobs_query_with_status() {
        let query: AdminListJobsQuery =
            serde_json::from_str(r#"{"limit": 10, "status": "failed"}"#).unwrap();
        assert_eq!(query.limit, Some(10));
        assert_eq!(query.status.as_deref(), Some("failed"));
    }

    #[test]
    fn limit_clamping_logic() {
        let clamp = |limit: Option<i64>| limit.unwrap_or(50).clamp(1, 100);
        assert_eq!(clamp(None), 50);
        assert_eq!(clamp(Some(200)), 100);
        assert_eq!(clamp(Some(0)), 1);
        assert_eq!(clamp(Some(-5)), 1);
        assert_eq!(clamp(Some(10)), 10);
    }

    #[test]
    fn admin_repo_detail_response_serializes() {
        use crate::repos::models::Visibility;
        use chrono::Utc;

        let response = AdminRepoDetailResponse {
            repo: RepoResponse {
                id: Uuid::nil(),
                owner_id: Uuid::nil(),
                org_id: Uuid::nil(),
                project_id: Uuid::nil(),
                name: "test".to_string(),
                slug: "test".to_string(),
                description: None,
                visibility: Visibility::Public,
                default_branch: "main".to_string(),
                archived: false,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            storage_exists: true,
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["storage_exists"], true);
        // Verify flatten works - repo fields are at top level
        assert!(json.get("id").is_some());
        assert!(json.get("visibility").is_some());
    }
}

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use crate::app_state::AppState;
use crate::auth::middleware::{OptionalAuth, RequireAuth};
use crate::errors::ApiError;
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::routes::resolve_repo;
use crate::storage;
use crate::storage::service::StorageConfig;

use super::service as branch_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `/repos/{org_id}/{repo}/branches`.
#[derive(Debug, Deserialize)]
pub struct OrgRepoPath {
    pub org_id: uuid::Uuid,
    pub repo: String,
}

/// Path parameters for `/repos/{org_id}/{repo}/branches/{branch}`.
/// The branch name may contain slashes (e.g. `feature/foo`), so we
/// capture it as a wildcard tail segment.
#[derive(Debug, Deserialize)]
pub struct OrgRepoBranchPath {
    pub org_id: uuid::Uuid,
    pub repo: String,
    pub branch: String,
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// JSON body for `POST /repos/{org_id}/{repo}/branches`.
#[derive(Debug, Deserialize)]
pub struct CreateBranchRequest {
    pub name: String,
    pub start_point: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `StorageConfig` from the shared application state.
fn storage_config(state: &AppState) -> StorageConfig {
    StorageConfig::new(state.git_storage_root.clone())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /repos/{org_id}/{repo}/branches -- List all branches (optional auth).
async fn list_branches(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
) -> Result<Json<Vec<crate::branches::models::BranchInfo>>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let sc = storage_config(&state);
    let branches = branch_service::list_branches(&sc, repo.id, &repo.default_branch).await?;

    Ok(Json(branches))
}

/// GET /repos/{org_id}/{repo}/branches/{*branch} -- Get a single branch (optional auth).
async fn get_branch(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoBranchPath>,
) -> Result<Json<crate::branches::models::BranchInfo>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let sc = storage_config(&state);
    let branch = branch_service::get_branch(&sc, repo.id, &path.branch, &repo.default_branch)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("branch '{}' not found", path.branch)))?;

    Ok(Json(branch))
}

/// POST /repos/{org_id}/{repo}/branches -- Create a branch (auth required, write access).
async fn create_branch(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
    Json(body): Json<CreateBranchRequest>,
) -> Result<(StatusCode, Json<crate::branches::models::BranchInfo>), ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check write permission (also rejects archived repos).
    permissions_service::check_repo_access(&state.db, Some(user.id), repo.id, Permission::Write)
        .await?;

    // Reject if repo is archived (belt-and-suspenders; check_repo_access
    // should already reject writes on archived repos).
    if repo.archived {
        return Err(ApiError::Forbidden("repository is archived".to_string()));
    }

    let sc = storage_config(&state);
    let branch = branch_service::create_branch(
        &sc,
        repo.id,
        &body.name,
        &body.start_point,
        &repo.default_branch,
    )
    .await?;

    // Emit audit event.
    storage::emit_audit_event(
        &state.db,
        user.id,
        "branch.created",
        Some(repo.id),
        None,
        Some(serde_json::json!({
            "branch": branch.name,
            "start_point": body.start_point,
        })),
    )
    .await;

    Ok((StatusCode::CREATED, Json(branch)))
}

/// DELETE /repos/{org_id}/{repo}/branches/{*branch} -- Delete a branch (auth required, write access).
async fn delete_branch(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoBranchPath>,
) -> Result<StatusCode, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check write permission (also rejects archived repos).
    permissions_service::check_repo_access(&state.db, Some(user.id), repo.id, Permission::Write)
        .await?;

    // Reject if repo is archived.
    if repo.archived {
        return Err(ApiError::Forbidden("repository is archived".to_string()));
    }

    let sc = storage_config(&state);
    branch_service::delete_branch(&sc, repo.id, &path.branch, &repo.default_branch).await?;

    // Emit audit event.
    storage::emit_audit_event(
        &state.db,
        user.id,
        "branch.deleted",
        Some(repo.id),
        None,
        Some(serde_json::json!({
            "branch": path.branch,
        })),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Router for branch management endpoints.
///
/// Mounts:
/// - `GET    /repos/{org_id}/{repo}/branches`           -- list branches
/// - `POST   /repos/{org_id}/{repo}/branches`           -- create branch
/// - `GET    /repos/{org_id}/{repo}/branches/{*branch}`  -- get branch details
/// - `DELETE /repos/{org_id}/{repo}/branches/{*branch}`  -- delete branch
pub fn branches_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{org_id}/{repo}/branches",
            get(list_branches).post(create_branch),
        )
        .route(
            "/repos/{org_id}/{repo}/branches/{*branch}",
            get(get_branch).delete(delete_branch),
        )
}

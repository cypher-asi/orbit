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
use crate::repos::service as repo_service;
use crate::storage;
use crate::storage::service::StorageConfig;
use crate::users::service as user_service;

use super::service as branch_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `/repos/{owner}/{repo}/branches`.
#[derive(Debug, Deserialize)]
pub struct OwnerRepoPath {
    pub owner: String,
    pub repo: String,
}

/// Path parameters for `/repos/{owner}/{repo}/branches/{branch}`.
/// The branch name may contain slashes (e.g. `feature/foo`), so we
/// capture it as a wildcard tail segment.
#[derive(Debug, Deserialize)]
pub struct OwnerRepoBranchPath {
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// JSON body for `POST /repos/{owner}/{repo}/branches`.
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

/// Resolve a repository from `{owner}/{repo}` path params.
///
/// Looks up the user by username, then the repo by `(owner_id, slug)`.
/// Returns `NotFound` if either the user or repo does not exist.
async fn resolve_repo(
    pool: &sqlx::PgPool,
    owner_name: &str,
    repo_slug: &str,
) -> Result<crate::repos::models::Repo, ApiError> {
    let owner = user_service::get_user_by_username(pool, owner_name)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    let repo = repo_service::get_repo_by_owner_and_slug(pool, owner.id, repo_slug)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    Ok(repo)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /repos/{owner}/{repo}/branches -- List all branches (optional auth).
async fn list_branches(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
) -> Result<Json<Vec<crate::branches::models::BranchInfo>>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(
        &state.db,
        viewer_id,
        repo.id,
        Permission::Read,
    )
    .await?;

    let sc = storage_config(&state);
    let branches = branch_service::list_branches(&sc, repo.id, &repo.default_branch).await?;

    Ok(Json(branches))
}

/// GET /repos/{owner}/{repo}/branches/{*branch} -- Get a single branch (optional auth).
async fn get_branch(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoBranchPath>,
) -> Result<Json<crate::branches::models::BranchInfo>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(
        &state.db,
        viewer_id,
        repo.id,
        Permission::Read,
    )
    .await?;

    let sc = storage_config(&state);
    let branch = branch_service::get_branch(&sc, repo.id, &path.branch, &repo.default_branch)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound(format!("branch '{}' not found", path.branch))
        })?;

    Ok(Json(branch))
}

/// POST /repos/{owner}/{repo}/branches -- Create a branch (auth required, write access).
async fn create_branch(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
    Json(body): Json<CreateBranchRequest>,
) -> Result<(StatusCode, Json<crate::branches::models::BranchInfo>), ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check write permission (also rejects archived repos).
    permissions_service::check_repo_access(
        &state.db,
        Some(user.id),
        repo.id,
        Permission::Write,
    )
    .await?;

    // Reject if repo is archived (belt-and-suspenders; check_repo_access
    // should already reject writes on archived repos).
    if repo.archived {
        return Err(ApiError::Forbidden(
            "repository is archived".to_string(),
        ));
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

/// DELETE /repos/{owner}/{repo}/branches/{*branch} -- Delete a branch (auth required, write access).
async fn delete_branch(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoBranchPath>,
) -> Result<StatusCode, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check write permission (also rejects archived repos).
    permissions_service::check_repo_access(
        &state.db,
        Some(user.id),
        repo.id,
        Permission::Write,
    )
    .await?;

    // Reject if repo is archived.
    if repo.archived {
        return Err(ApiError::Forbidden(
            "repository is archived".to_string(),
        ));
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
/// - `GET    /repos/{owner}/{repo}/branches`           -- list branches
/// - `POST   /repos/{owner}/{repo}/branches`           -- create branch
/// - `GET    /repos/{owner}/{repo}/branches/{*branch}`  -- get branch details
/// - `DELETE /repos/{owner}/{repo}/branches/{*branch}`  -- delete branch
pub fn branches_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/branches",
            get(list_branches).post(create_branch),
        )
        .route(
            "/repos/{owner}/{repo}/branches/{*branch}",
            get(get_branch).delete(delete_branch),
        )
}

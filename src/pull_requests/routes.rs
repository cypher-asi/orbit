use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::middleware::{OptionalAuth, RequireAuth};
use crate::errors::ApiError;
use crate::merge_engine::models::ConflictCheck;
use crate::merge_engine::service as merge_service;
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::routes::resolve_repo;
use crate::storage::service::StorageConfig;

use super::models::{
    CreatePrInput, MergeabilityState, PrFilter, PrStatus, PullRequest, UpdatePrInput,
};
use super::service as pr_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `/repos/{org_id}/{repo}/pulls`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRepoPath {
    pub org_id: Uuid,
    pub repo: String,
}

/// Path parameters for `/repos/{org_id}/{repo}/pulls/{id}`.
/// `id` is the pull request's UUID.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRepoPrPath {
    pub org_id: Uuid,
    pub repo: String,
    pub id: Uuid,
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Query parameters for `GET /repos/{org_id}/{repo}/pulls`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPrsQuery {
    pub status: Option<String>,
    pub author_id: Option<Uuid>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

// ---------------------------------------------------------------------------
// Request body types
// ---------------------------------------------------------------------------

/// JSON body for `POST /repos/{org_id}/{repo}/pulls`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePrRequest {
    pub source_branch: String,
    pub target_branch: String,
    pub title: String,
    pub description: Option<String>,
}

/// JSON body for `PATCH /repos/{org_id}/{repo}/pulls/{id}`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePrRequest {
    pub title: Option<String>,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `StorageConfig` from the shared application state.
fn storage_config(state: &AppState) -> StorageConfig {
    StorageConfig::new(state.git_storage_root.clone())
}

/// Resolve a pull request by UUID and verify it belongs to the given repo.
/// Returns 404 if the PR does not exist or belongs to another repo.
async fn resolve_pr_in_repo(
    pool: &sqlx::PgPool,
    pr_id: Uuid,
    repo_id: Uuid,
) -> Result<PullRequest, ApiError> {
    let pr = pr_service::get_pr_by_id(pool, pr_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("pull request not found".to_string()))?;
    if pr.repo_id != repo_id {
        return Err(ApiError::NotFound("pull request not found".to_string()));
    }
    Ok(pr)
}

/// Check that the current user is either the PR author or has write access to the repo.
///
/// This implements the "author or write" access pattern used for update, close,
/// and reopen operations. The PR author can always modify their own PR, and
/// users with write (or higher) permission on the repo can modify any PR.
async fn check_author_or_write(
    state: &AppState,
    user_id: uuid::Uuid,
    repo_id: uuid::Uuid,
    author_id: uuid::Uuid,
) -> Result<(), ApiError> {
    // If the user is the PR author, allow immediately.
    if user_id == author_id {
        return Ok(());
    }

    // Otherwise, require write permission on the repo.
    permissions_service::check_repo_access(&state.db, Some(user_id), repo_id, Permission::Write)
        .await
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /repos/{org_id}/{repo}/pulls -- Create a new pull request (auth required, write access).
async fn create_pr(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
    Json(body): Json<CreatePrRequest>,
) -> Result<(StatusCode, Json<PullRequest>), ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check write permission.
    permissions_service::check_repo_access(&state.db, Some(user.id), repo.id, Permission::Write)
        .await?;

    let sc = storage_config(&state);
    let input = CreatePrInput {
        repo_id: repo.id,
        author_id: user.id,
        source_branch: body.source_branch.clone(),
        target_branch: body.target_branch.clone(),
        title: body.title.clone(),
        description: body.description.clone(),
    };

    let pr = pr_service::create_pr(&state.db, &sc, input).await?;

    Ok((StatusCode::CREATED, Json(pr)))
}

/// GET /repos/{org_id}/{repo}/pulls -- List pull requests (optional auth).
async fn list_prs(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
    Query(query): Query<ListPrsQuery>,
) -> Result<Json<Vec<PullRequest>>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    // Parse the optional status filter.
    let status = match query.status.as_deref() {
        Some(s) => {
            let parsed = PrStatus::from_db_str(s)
                .ok_or_else(|| ApiError::BadRequest(format!("invalid status filter: '{}'", s)))?;
            Some(parsed)
        }
        None => None,
    };

    let filter = PrFilter {
        status,
        author_id: query.author_id,
        limit: query.limit.unwrap_or(20),
        offset: query.offset.unwrap_or(0),
    };

    let prs = pr_service::list_prs(&state.db, repo.id, filter).await?;

    Ok(Json(prs))
}

/// GET /repos/{org_id}/{repo}/pulls/{id} -- Get PR details (optional auth).
async fn get_pr(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPrPath>,
) -> Result<Json<PullRequest>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let pr = resolve_pr_in_repo(&state.db, path.id, repo.id).await?;

    Ok(Json(pr))
}

/// PATCH /repos/{org_id}/{repo}/pulls/{id} -- Update PR title/description (author or write access).
async fn update_pr(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPrPath>,
    Json(body): Json<UpdatePrRequest>,
) -> Result<Json<PullRequest>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    let pr = resolve_pr_in_repo(&state.db, path.id, repo.id).await?;

    // Author or write access required.
    check_author_or_write(&state, user.id, repo.id, pr.author_id).await?;

    let input = UpdatePrInput {
        title: body.title.clone(),
        description: body.description.clone(),
    };

    let updated = pr_service::update_pr(&state.db, pr.id, user.id, input).await?;

    Ok(Json(updated))
}

/// POST /repos/{org_id}/{repo}/pulls/{id}/close -- Close a PR (author or write access).
async fn close_pr(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPrPath>,
) -> Result<Json<PullRequest>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    let pr = resolve_pr_in_repo(&state.db, path.id, repo.id).await?;

    // Author or write access required.
    check_author_or_write(&state, user.id, repo.id, pr.author_id).await?;

    let closed = pr_service::close_pr(&state.db, pr.id, user.id).await?;

    Ok(Json(closed))
}

/// POST /repos/{org_id}/{repo}/pulls/{id}/reopen -- Reopen a PR (author or write access).
async fn reopen_pr(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPrPath>,
) -> Result<Json<PullRequest>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    let pr = resolve_pr_in_repo(&state.db, path.id, repo.id).await?;

    // Author or write access required.
    check_author_or_write(&state, user.id, repo.id, pr.author_id).await?;

    let sc = storage_config(&state);
    let reopened = pr_service::reopen_pr(&state.db, &sc, pr.id, user.id).await?;

    Ok(Json(reopened))
}

/// GET /repos/{org_id}/{repo}/pulls/{id}/diff -- Get PR diff (optional auth).
async fn get_pr_diff(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPrPath>,
) -> Result<String, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let pr = resolve_pr_in_repo(&state.db, path.id, repo.id).await?;

    let diff = pr_service::get_pr_diff(
        &storage_config(&state),
        repo.id,
        &pr.source_branch,
        &pr.target_branch,
    )
    .await?;

    Ok(diff)
}

/// GET /repos/{org_id}/{repo}/pulls/{id}/mergeability -- Check mergeability (optional auth).
async fn check_mergeability(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPrPath>,
) -> Result<Json<MergeabilityResponse>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let pr = resolve_pr_in_repo(&state.db, path.id, repo.id).await?;

    let state_val = pr_service::check_mergeability(
        &storage_config(&state),
        repo.id,
        &pr.source_branch,
        &pr.target_branch,
    )
    .await?;

    Ok(Json(MergeabilityResponse {
        mergeability: state_val,
    }))
}

/// GET /repos/{org_id}/{repo}/pulls/{id}/conflicts -- Check merge conflicts (optional auth).
///
/// Preview merge conflicts between the PR's source and target branches
/// without performing the actual merge. Returns a `ConflictCheck` indicating
/// whether conflicts exist and which files are affected.
async fn check_conflicts(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPrPath>,
) -> Result<Json<ConflictCheck>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let pr = resolve_pr_in_repo(&state.db, path.id, repo.id).await?;

    let sc = storage_config(&state);
    let conflict_check =
        merge_service::check_conflicts(&sc, repo.id, &pr.source_branch, &pr.target_branch).await?;

    Ok(Json(conflict_check))
}

// NOTE: The merge PR handler has been moved to merge_engine::routes.

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// JSON response for the mergeability check endpoint.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeabilityResponse {
    pub mergeability: MergeabilityState,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Return a method router for the create-PR handler.
///
/// This allows the central router to mount the creation endpoint separately
/// with its own rate-limit layer.
pub fn create_pr_handler() -> axum::routing::MethodRouter<AppState> {
    post(create_pr)
}

/// Build a Router for pull request endpoints excluding `POST /repos/{org_id}/{repo}/pulls` (create).
///
/// Used by the central router to separate rate-limited PR creation from
/// non-rate-limited read/update operations.
///
/// Mounts:
/// - `GET    /repos/{org_id}/{repo}/pulls`                  -- list PRs
/// - `GET    /repos/{org_id}/{repo}/pulls/{id}`             -- get PR details (id = UUID)
/// - `PATCH  /repos/{org_id}/{repo}/pulls/{id}`             -- update PR
/// - `POST   /repos/{org_id}/{repo}/pulls/{id}/close`       -- close PR
/// - `POST   /repos/{org_id}/{repo}/pulls/{id}/reopen`      -- reopen PR
/// - `GET    /repos/{org_id}/{repo}/pulls/{id}/diff`        -- get PR diff
/// - `GET    /repos/{org_id}/{repo}/pulls/{id}/mergeability` -- check mergeability
/// - `GET    /repos/{org_id}/{repo}/pulls/{id}/conflicts`   -- check merge conflicts
pub fn pull_request_routes_without_create() -> Router<AppState> {
    Router::new()
        .route("/repos/{org_id}/{repo}/pulls", get(list_prs))
        .route(
            "/repos/{org_id}/{repo}/pulls/{id}",
            get(get_pr).patch(update_pr),
        )
        .route("/repos/{org_id}/{repo}/pulls/{id}/close", post(close_pr))
        .route("/repos/{org_id}/{repo}/pulls/{id}/reopen", post(reopen_pr))
        .route("/repos/{org_id}/{repo}/pulls/{id}/diff", get(get_pr_diff))
        .route(
            "/repos/{org_id}/{repo}/pulls/{id}/mergeability",
            get(check_mergeability),
        )
        .route(
            "/repos/{org_id}/{repo}/pulls/{id}/conflicts",
            get(check_conflicts),
        )
}

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
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::models::{CreateRepoInput, Pagination, Repo, RepoResponse, UpdateRepoInput};
use crate::repos::service;
use crate::users::service as user_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `{owner}/{repo}` routes.
#[derive(Debug, Deserialize)]
pub struct OwnerRepoPath {
    pub owner: String,
    pub repo: String,
}

/// Path parameters for `{username}` routes.
#[derive(Debug, Deserialize)]
pub struct UsernamePath {
    pub username: String,
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Pagination query parameters (limit/offset).
#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl PaginationQuery {
    fn into_pagination(self) -> Pagination {
        Pagination {
            limit: self.limit.unwrap_or(20),
            offset: self.offset.unwrap_or(0),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a repository from `{owner}/{repo}` path params.
///
/// Looks up the user by username, then the repo by `(owner_id, slug)`.
/// Returns `NotFound` if either the user or repo does not exist.
async fn resolve_repo(
    pool: &sqlx::PgPool,
    owner_name: &str,
    repo_slug: &str,
) -> Result<(Uuid, Repo), ApiError> {
    let owner = user_service::get_user_by_username(pool, owner_name)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    let repo = service::get_repo_by_owner_and_slug(pool, owner.id, repo_slug)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    Ok((owner.id, repo))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /repos -- Create a new repository (auth required).
async fn create_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Json(input): Json<CreateRepoInput>,
) -> Result<(StatusCode, Json<RepoResponse>), ApiError> {
    let repo = service::create_repo(
        &state.db,
        &state.git_storage_root,
        user.id,
        input,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(RepoResponse::from(repo))))
}

/// GET /repos/{owner}/{repo} -- Get repository metadata (optional auth).
///
/// Resolves the owner by username, then the repo by slug.
/// Checks that the viewer has read permission before returning metadata.
async fn get_repo(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
) -> Result<Json<RepoResponse>, ApiError> {
    let (_owner_id, repo) = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(
        &state.db,
        viewer_id,
        repo.id,
        Permission::Read,
    )
    .await?;

    Ok(Json(RepoResponse::from(repo)))
}

/// PATCH /repos/{owner}/{repo} -- Update repository metadata (owner required).
async fn update_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
    Json(input): Json<UpdateRepoInput>,
) -> Result<Json<RepoResponse>, ApiError> {
    let (_owner_id, repo) = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check admin permission (only owner can update metadata).
    permissions_service::check_repo_access(
        &state.db,
        Some(user.id),
        repo.id,
        Permission::Admin,
    )
    .await?;

    let updated = service::update_repo(&state.db, repo.id, input).await?;

    Ok(Json(RepoResponse::from(updated)))
}

/// GET /repos -- List repositories accessible to the current user (auth required).
///
/// Returns repos that are owned by the user, where the user is a collaborator,
/// or that are public. Supports `limit` and `offset` query parameters.
async fn list_accessible_repos(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    let repos = service::list_accessible_repos(
        &state.db,
        user.id,
        pagination.into_pagination(),
    )
    .await?;

    let responses: Vec<RepoResponse> = repos.into_iter().map(RepoResponse::from).collect();
    Ok(Json(responses))
}

/// GET /users/{username}/repos -- List a user's repos visible to the viewer (optional auth).
///
/// If the viewer is the same user, all repos are returned.
/// Otherwise, only public repos are returned.
async fn list_user_repos(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<UsernamePath>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    let target_user = user_service::get_user_by_username(&state.db, &path.username)
        .await?
        .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;

    let viewer_id = user.as_ref().map(|u| u.id);
    let repos = service::list_repos_for_user(
        &state.db,
        target_user.id,
        viewer_id,
        pagination.into_pagination(),
    )
    .await?;

    let responses: Vec<RepoResponse> = repos.into_iter().map(RepoResponse::from).collect();
    Ok(Json(responses))
}

/// POST /repos/{owner}/{repo}/archive -- Archive a repository (owner required).
async fn archive_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
) -> Result<StatusCode, ApiError> {
    let (_owner_id, repo) = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check admin permission (only owner can archive).
    permissions_service::check_repo_access(
        &state.db,
        Some(user.id),
        repo.id,
        Permission::Admin,
    )
    .await?;

    service::archive_repo(&state.db, repo.id, user.id).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /repos/{owner}/{repo} -- Soft-delete a repository (owner required).
async fn delete_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
) -> Result<StatusCode, ApiError> {
    let (_owner_id, repo) = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check admin permission (only owner can delete).
    permissions_service::check_repo_access(
        &state.db,
        Some(user.id),
        repo.id,
        Permission::Admin,
    )
    .await?;

    service::delete_repo(&state.db, repo.id, user.id).await?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Return a method router for the create-repo handler.
///
/// This allows the central router to mount the creation endpoint separately
/// with its own rate-limit layer.
pub fn create_repo_handler() -> axum::routing::MethodRouter<AppState> {
    post(create_repo)
}

/// Build a Router for repo endpoints excluding `POST /repos` (create).
///
/// Used by the central router to separate rate-limited creation from
/// non-rate-limited CRUD routes.
///
/// Mounts:
/// - `GET    /repos`                           -- list accessible repos
/// - `GET    /repos/{owner}/{repo}`            -- get repo metadata
/// - `PATCH  /repos/{owner}/{repo}`            -- update repo metadata
/// - `POST   /repos/{owner}/{repo}/archive`    -- archive repo
/// - `DELETE /repos/{owner}/{repo}`            -- soft-delete repo
/// - `GET    /users/{username}/repos`          -- list user's repos
pub fn repo_routes_without_create() -> Router<AppState> {
    Router::new()
        .route("/repos", get(list_accessible_repos))
        .route(
            "/repos/{owner}/{repo}",
            get(get_repo).patch(update_repo).delete(delete_repo),
        )
        .route("/repos/{owner}/{repo}/archive", post(archive_repo))
        .route("/users/{username}/repos", get(list_user_repos))
}

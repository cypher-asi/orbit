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
use crate::repos::models::{CreateRepoInput, Pagination, RepoResponse, UpdateRepoInput};
use crate::repos::service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `{org_id}/{repo}` routes.
#[derive(Debug, Deserialize)]
pub struct OrgRepoPath {
    pub org_id: Uuid,
    pub repo: String,
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

/// Resolve a repository from `{org_id}/{repo}` path params.
///
/// Looks up the repo by `(org_id, slug)`.
/// Returns `NotFound` if the repo does not exist.
pub async fn resolve_repo(
    pool: &sqlx::PgPool,
    org_id: Uuid,
    repo_slug: &str,
) -> Result<crate::repos::models::Repo, ApiError> {
    service::get_repo_by_org_and_slug(pool, org_id, repo_slug)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /repos -- Create a new repository (auth required).
///
/// Body must include `orgId` and `projectId` to link to aura-network.
async fn create_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Json(input): Json<CreateRepoWithContext>,
) -> Result<(StatusCode, Json<RepoResponse>), ApiError> {
    let repo = service::create_repo(
        &state.db,
        &state.git_storage_root,
        user.id,
        input.org_id,
        input.project_id,
        input.repo,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(RepoResponse::from(repo))))
}

/// Request body for creating a repo with org/project context.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRepoWithContext {
    pub org_id: Uuid,
    pub project_id: Uuid,
    #[serde(flatten)]
    pub repo: CreateRepoInput,
}

/// GET /repos/{org_id}/{repo} -- Get repository metadata (optional auth).
async fn get_repo(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
) -> Result<Json<RepoResponse>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    Ok(Json(RepoResponse::from(repo)))
}

/// PATCH /repos/{org_id}/{repo} -- Update repository metadata (owner required).
async fn update_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
    Json(input): Json<UpdateRepoInput>,
) -> Result<Json<RepoResponse>, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    permissions_service::check_repo_access(&state.db, Some(user.id), repo.id, Permission::Admin)
        .await?;

    let updated = service::update_repo(&state.db, repo.id, input).await?;

    Ok(Json(RepoResponse::from(updated)))
}

/// GET /repos -- List repositories accessible to the current user (auth required).
async fn list_accessible_repos(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    let repos =
        service::list_accessible_repos(&state.db, user.id, pagination.into_pagination()).await?;

    let responses: Vec<RepoResponse> = repos.into_iter().map(RepoResponse::from).collect();
    Ok(Json(responses))
}

/// POST /repos/{org_id}/{repo}/archive -- Archive a repository (owner required).
async fn archive_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
) -> Result<StatusCode, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    permissions_service::check_repo_access(&state.db, Some(user.id), repo.id, Permission::Admin)
        .await?;

    service::archive_repo(&state.db, repo.id, user.id).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /repos/{org_id}/{repo} -- Soft-delete a repository (owner required).
async fn delete_repo(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<OrgRepoPath>,
) -> Result<StatusCode, ApiError> {
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    permissions_service::check_repo_access(&state.db, Some(user.id), repo.id, Permission::Admin)
        .await?;

    service::delete_repo(&state.db, repo.id, user.id).await?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Return a method router for the create-repo handler.
pub fn create_repo_handler() -> axum::routing::MethodRouter<AppState> {
    post(create_repo)
}

/// Build a Router for repo endpoints excluding `POST /repos` (create).
pub fn repo_routes_without_create() -> Router<AppState> {
    Router::new()
        .route("/repos", get(list_accessible_repos))
        .route(
            "/repos/{org_id}/{repo}",
            get(get_repo).patch(update_repo).delete(delete_repo),
        )
        .route("/repos/{org_id}/{repo}/archive", post(archive_repo))
}

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use crate::app_state::AppState;
use crate::auth::middleware::OptionalAuth;
use crate::errors::ApiError;
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::service as repo_service;
use crate::storage::service::StorageConfig;
use crate::users::service as user_service;

use super::models::TagInfo;
use super::service as tags_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OwnerRepoPath {
    pub owner: String,
    pub repo: String,
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListTagsQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn storage_config(state: &AppState) -> StorageConfig {
    StorageConfig::new(state.git_storage_root.clone())
}

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

/// GET /repos/{owner}/{repo}/tags — List tags (optional auth).
///
/// Returns tag name, target SHA, and optionally peeled SHA for annotated tags.
/// Query: limit (default 100, max 100), offset (default 0).
async fn list_tags(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
    Query(query): Query<ListTagsQuery>,
) -> Result<Json<Vec<TagInfo>>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let limit = query.limit.unwrap_or(100).min(100);
    let offset = query.offset.unwrap_or(0);

    let sc = storage_config(&state);
    let all_tags = tags_service::list_tags(&sc, repo.id).await?;

    let total = all_tags.len() as u32;
    let start = (offset as usize).min(total as usize);
    let end = (start + limit as usize).min(total as usize);
    let page: Vec<TagInfo> = all_tags[start..end].to_vec();

    Ok(Json(page))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Router for tag list endpoint.
///
/// Mounts:
/// - `GET /repos/{owner}/{repo}/tags` — list tags (paginated)
pub fn tags_routes() -> Router<AppState> {
    Router::new().route("/repos/{owner}/{repo}/tags", get(list_tags))
}

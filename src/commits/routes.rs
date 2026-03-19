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

use super::models::{CommitInfo, DiffEntry, FileContent, TreeEntry};
use super::service as commit_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `/repos/{owner}/{repo}/commits`.
#[derive(Debug, Deserialize)]
pub struct OwnerRepoPath {
    pub owner: String,
    pub repo: String,
}

/// Path parameters for `/repos/{owner}/{repo}/commits/{sha}`.
#[derive(Debug, Deserialize)]
pub struct OwnerRepoShaPath {
    pub owner: String,
    pub repo: String,
    pub sha: String,
}

/// Path parameters for `/repos/{owner}/{repo}/tree/{ref}/{*path}` and
/// `/repos/{owner}/{repo}/blob/{ref}/{*path}`.
#[derive(Debug, Deserialize)]
pub struct OwnerRepoRefPath {
    pub owner: String,
    pub repo: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Query parameters for `GET /repos/{owner}/{repo}/commits`.
#[derive(Debug, Deserialize)]
pub struct ListCommitsQuery {
    /// Branch, tag, or SHA to list commits from. Defaults to the repo's
    /// default branch.
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
    /// Maximum number of commits to return (default 30, max 100).
    pub limit: Option<u32>,
    /// Number of commits to skip (for pagination).
    pub offset: Option<u32>,
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

/// GET /repos/{owner}/{repo}/commits -- List commits for a branch or ref.
async fn list_commits(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoPath>,
    Query(query): Query<ListCommitsQuery>,
) -> Result<Json<Vec<CommitInfo>>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let ref_name = query
        .ref_name
        .unwrap_or_else(|| repo.default_branch.clone());

    // Clamp limit to 1..=100, default 30.
    let limit = query.limit.unwrap_or(30).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);

    let sc = storage_config(&state);
    let commits = commit_service::list_commits(&sc, repo.id, &ref_name, limit, offset).await?;

    Ok(Json(commits))
}

/// GET /repos/{owner}/{repo}/commits/{sha} -- Get commit details.
async fn get_commit(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoShaPath>,
) -> Result<Json<CommitInfo>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let sc = storage_config(&state);
    let commit = commit_service::get_commit(&sc, repo.id, &path.sha)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("commit '{}' not found", path.sha)))?;

    Ok(Json(commit))
}

/// GET /repos/{owner}/{repo}/tree/{ref}/{*path} -- Browse repository tree.
async fn browse_tree(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoRefPath>,
) -> Result<Json<Vec<TreeEntry>>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let tree_path = if path.path.is_empty() {
        None
    } else {
        Some(path.path.as_str())
    };

    let sc = storage_config(&state);
    let entries = commit_service::list_tree(&sc, repo.id, &path.ref_name, tree_path).await?;

    Ok(Json(entries))
}

/// GET /repos/{owner}/{repo}/blob/{ref}/{*path} -- Get file content.
async fn get_blob(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoRefPath>,
) -> Result<Json<FileContent>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    if path.path.is_empty() {
        return Err(ApiError::BadRequest("path is required".to_string()));
    }

    let sc = storage_config(&state);
    let content =
        commit_service::get_file_content(&sc, repo.id, &path.ref_name, &path.path).await?;

    Ok(Json(content))
}

/// GET /repos/{owner}/{repo}/commits/{sha}/diff -- Get commit diff.
async fn get_commit_diff(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<OwnerRepoShaPath>,
) -> Result<Json<Vec<DiffEntry>>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Check read permission.
    let viewer_id = user.as_ref().map(|u| u.id);
    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read).await?;

    let sc = storage_config(&state);
    let diff = commit_service::get_commit_diff(&sc, repo.id, &path.sha).await?;

    Ok(Json(diff))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Router for commit history and repository browsing endpoints.
///
/// Mounts:
/// - `GET /repos/{owner}/{repo}/commits`              -- list commits
/// - `GET /repos/{owner}/{repo}/commits/{sha}`         -- get commit details
/// - `GET /repos/{owner}/{repo}/commits/{sha}/diff`    -- get commit diff
/// - `GET /repos/{owner}/{repo}/tree/{ref}/{*path}`    -- browse tree
/// - `GET /repos/{owner}/{repo}/blob/{ref}/{*path}`    -- get file content
pub fn commits_routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/commits", get(list_commits))
        // Note: the diff route must come before the {sha} route so that
        // `{sha}/diff` is matched correctly by axum.
        .route(
            "/repos/{owner}/{repo}/commits/{sha}/diff",
            get(get_commit_diff),
        )
        .route("/repos/{owner}/{repo}/commits/{sha}", get(get_commit))
        .route("/repos/{owner}/{repo}/tree/{ref}/{*path}", get(browse_tree))
        .route("/repos/{owner}/{repo}/blob/{ref}/{*path}", get(get_blob))
}

use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use serde::Deserialize;

use crate::app_state::AppState;
use crate::auth::middleware::RequireAuth;
use crate::errors::ApiError;
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::service as repo_service;
use crate::storage;
use crate::storage::service::StorageConfig;
use crate::users::service as user_service;

use super::models::{MergeRequest, MergeResult};
use super::service as merge_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `/repos/{owner}/{repo}/pulls/{number}/merge`.
#[derive(Debug, Deserialize)]
pub struct MergePrPath {
    pub owner: String,
    pub repo: String,
    pub number: i32,
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
// Handler
// ---------------------------------------------------------------------------

/// POST /repos/{owner}/{repo}/pulls/{number}/merge -- Merge a PR (write access required).
///
/// Accepts a JSON body with `strategy` and optional `commit_message`.
/// Resolves the repository and PR, checks write permission, then delegates
/// to `merge_engine::service::merge_pr`.
///
/// ## Responses
///
/// - **200** -- Merge succeeded; returns `MergeResult` with commit SHA, strategy, and timestamp.
/// - **409** -- Merge conflicts detected (includes conflicting file list) or merge already in progress.
/// - **422** -- PR is not open, or source/target branch is missing.
/// - **500** -- Internal git error.
///
/// Emits `pr.merged` audit event on success and `merge.failed` on failure.
async fn merge_pr(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<MergePrPath>,
    Json(body): Json<MergeRequest>,
) -> Result<Json<MergeResult>, ApiError> {
    let repo = resolve_repo(&state.db, &path.owner, &path.repo).await?;

    // Require write permission on the repo.
    permissions_service::check_repo_access(
        &state.db,
        Some(user.id),
        repo.id,
        Permission::Write,
    )
    .await?;

    let sc = storage_config(&state);
    let result = merge_service::merge_pr(
        &state.db,
        &sc,
        repo.id,
        path.number,
        body.strategy,
        user.id,
        body.commit_message.clone(),
    )
    .await;

    match result {
        Ok(merge_result) => {
            // Success audit event is already emitted inside merge_service::merge_pr
            // (pr.merged and merge.completed). Return the result.
            Ok(Json(merge_result))
        }
        Err(ref err) => {
            // Emit merge.failed audit event for all failure cases.
            let error_message = format!("{}", err);
            storage::emit_audit_event(
                &state.db,
                user.id,
                "merge.failed",
                Some(repo.id),
                None,
                Some(serde_json::json!({
                    "pr_number": path.number,
                    "strategy": body.strategy.as_str(),
                    "error": error_message,
                })),
            )
            .await;

            // Return the original error.
            result.map(Json)
        }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Router for merge engine endpoints.
///
/// Mounts:
/// - `POST /repos/{owner}/{repo}/pulls/{number}/merge` -- merge a PR
pub fn merge_engine_routes() -> Router<AppState> {
    Router::new().route(
        "/repos/{owner}/{repo}/pulls/{number}/merge",
        post(merge_pr),
    )
}

/// Return a method router for the merge-PR handler.
///
/// This allows the central router to mount the merge endpoint separately
/// with its own rate-limit layer.
pub fn merge_pr_handler() -> axum::routing::MethodRouter<AppState> {
    post(merge_pr)
}

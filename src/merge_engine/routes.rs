use axum::{
    extract::{Path, State},
    routing::post,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::middleware::RequireAuth;
use crate::errors::ApiError;
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::routes::resolve_repo;
use crate::storage;
use crate::storage::service::StorageConfig;

use super::models::{MergeRequest, MergeResult};
use super::service as merge_service;

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for `/repos/{org_id}/{repo}/pulls/{id}/merge`.
/// `id` is the pull request's UUID.
#[derive(Debug, Deserialize)]
pub struct MergePrPath {
    pub org_id: Uuid,
    pub repo: String,
    pub id: Uuid,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `StorageConfig` from the shared application state.
fn storage_config(state: &AppState) -> StorageConfig {
    StorageConfig::new(state.git_storage_root.clone())
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// POST /repos/{org_id}/{repo}/pulls/{id}/merge -- Merge a PR (write access required).
///
/// Accepts a JSON body with `strategy` and optional `commit_message`.
/// Resolves the repository and PR by UUID, checks write permission, then delegates
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
    let repo = resolve_repo(&state.db, path.org_id, &path.repo).await?;

    // Require write permission on the repo.
    permissions_service::check_repo_access(&state.db, Some(user.id), repo.id, Permission::Write)
        .await?;

    // Verify the PR belongs to this repo (404 if not).
    let pr = crate::pull_requests::service::get_pr_by_id(&state.db, path.id)
        .await?
        .ok_or_else(|| ApiError::NotFound("pull request not found".to_string()))?;
    if pr.repo_id != repo.id {
        return Err(ApiError::NotFound("pull request not found".to_string()));
    }

    let sc = storage_config(&state);
    let result = merge_service::merge_pr(
        &state.db,
        &sc,
        path.id,
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
                Some(path.id),
                Some(serde_json::json!({
                    "pr_id": path.id,
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

/// Return a method router for the merge-PR handler.
///
/// This allows the central router to mount the merge endpoint separately
/// with its own rate-limit layer.
pub fn merge_pr_handler() -> axum::routing::MethodRouter<AppState> {
    post(merge_pr)
}

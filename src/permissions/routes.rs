use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;

use crate::app_state::AppState;
use crate::auth::AuthUser;
use crate::errors::ApiError;
use crate::permissions::models::{Permission, RepoMember, Role};
use crate::permissions::service;
use crate::users::service as user_service;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// JSON body for PUT /repos/{owner}/{repo}/collaborators/{username}
#[derive(Debug, Deserialize)]
pub struct AddOrUpdateCollaboratorRequest {
    pub role: Role,
}

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for repo-level collaborator routes.
#[derive(Debug, Deserialize)]
pub struct RepoCollabPath {
    pub owner: String,
    pub repo: String,
    pub username: String,
}

/// Path parameters for listing collaborators (no username).
#[derive(Debug, Deserialize)]
pub struct RepoPath {
    pub owner: String,
    pub repo: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a repo from the `{owner}/{repo}` path segments.
/// Returns the repo ID after verifying the caller has Admin permission.
async fn resolve_repo_for_admin(
    pool: &sqlx::PgPool,
    caller_id: uuid::Uuid,
    owner_name: &str,
    repo_slug: &str,
) -> Result<uuid::Uuid, ApiError> {
    // Look up the owner user by username.
    let owner = user_service::get_user_by_username(pool, owner_name)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    // Look up the repo by owner + slug.
    let repo_id = sqlx::query_scalar::<_, uuid::Uuid>(
        r#"
        SELECT id FROM repos
        WHERE owner_id = $1 AND slug = $2 AND deleted_at IS NULL
        "#,
    )
    .bind(owner.id)
    .bind(repo_slug)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "resolve_repo_for_admin: database error");
        ApiError::Internal("internal server error".to_string())
    })?
    .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    // Enforce Admin permission (only owners can manage collaborators).
    service::check_repo_access(pool, Some(caller_id), repo_id, Permission::Admin).await?;

    Ok(repo_id)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /repos/{owner}/{repo}/collaborators
///
/// List all collaborators of a repository. Requires Admin (owner) permission.
pub async fn list_collaborators(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(path): Path<RepoPath>,
) -> Result<Json<Vec<RepoMember>>, ApiError> {
    let repo_id =
        resolve_repo_for_admin(&state.db, user.id, &path.owner, &path.repo).await?;

    let members = service::list_collaborators(&state.db, repo_id).await?;
    Ok(Json(members))
}

/// PUT /repos/{owner}/{repo}/collaborators/{username}
///
/// Add or update a collaborator. Requires Admin (owner) permission.
/// The request body must contain a `role` field.
pub async fn add_or_update_collaborator(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(path): Path<RepoCollabPath>,
    Json(body): Json<AddOrUpdateCollaboratorRequest>,
) -> Result<(StatusCode, Json<RepoMember>), ApiError> {
    let repo_id =
        resolve_repo_for_admin(&state.db, user.id, &path.owner, &path.repo).await?;

    // Resolve the target user by username.
    let target_user = user_service::get_user_by_username(&state.db, &path.username)
        .await?
        .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;

    // Check if the target user already has a membership.
    let existing_role =
        service::get_user_role(&state.db, target_user.id, repo_id).await?;

    match existing_role {
        Some(_) => {
            // Update existing collaborator's role.
            let member = service::update_collaborator_role(
                &state.db,
                repo_id,
                target_user.id,
                body.role,
            )
            .await?;
            Ok((StatusCode::OK, Json(member)))
        }
        None => {
            // Add new collaborator.
            let member = service::add_collaborator(
                &state.db,
                repo_id,
                target_user.id,
                body.role,
            )
            .await?;
            Ok((StatusCode::CREATED, Json(member)))
        }
    }
}

/// DELETE /repos/{owner}/{repo}/collaborators/{username}
///
/// Remove a collaborator. Requires Admin (owner) permission.
/// Cannot remove the repository owner.
pub async fn remove_collaborator(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(path): Path<RepoCollabPath>,
) -> Result<StatusCode, ApiError> {
    let repo_id =
        resolve_repo_for_admin(&state.db, user.id, &path.owner, &path.repo).await?;

    // Resolve the target user by username.
    let target_user = user_service::get_user_by_username(&state.db, &path.username)
        .await?
        .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;

    service::remove_collaborator(&state.db, repo_id, target_user.id).await?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Router for collaborator management endpoints.
///
/// Mounts:
/// - `GET    /repos/{owner}/{repo}/collaborators`
/// - `PUT    /repos/{owner}/{repo}/collaborators/{username}`
/// - `DELETE /repos/{owner}/{repo}/collaborators/{username}`
pub fn collaborator_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/collaborators",
            get(list_collaborators),
        )
        .route(
            "/repos/{owner}/{repo}/collaborators/{username}",
            put(add_or_update_collaborator).delete(remove_collaborator),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_add_collaborator_request() {
        let json = r#"{"role": "writer"}"#;
        let req: AddOrUpdateCollaboratorRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.role, Role::Writer);
    }

    #[test]
    fn deserialize_add_collaborator_request_reader() {
        let json = r#"{"role": "reader"}"#;
        let req: AddOrUpdateCollaboratorRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.role, Role::Reader);
    }

    #[test]
    fn deserialize_add_collaborator_request_owner() {
        let json = r#"{"role": "owner"}"#;
        let req: AddOrUpdateCollaboratorRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.role, Role::Owner);
    }

    #[test]
    fn deserialize_add_collaborator_request_invalid() {
        let json = r#"{"role": "superadmin"}"#;
        let result: Result<AddOrUpdateCollaboratorRequest, _> =
            serde_json::from_str(json);
        assert!(result.is_err());
    }
}

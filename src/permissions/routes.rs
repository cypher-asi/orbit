use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::middleware::RequireAuth;
use crate::errors::ApiError;
use crate::permissions::models::{Permission, RepoMember, Role};
use crate::permissions::service;
use crate::repos::routes::resolve_repo;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// JSON body for PUT /repos/{org_id}/{repo}/collaborators/{user_id}
#[derive(Debug, Deserialize)]
pub struct AddOrUpdateCollaboratorRequest {
    pub role: Role,
}

// ---------------------------------------------------------------------------
// Path extractors
// ---------------------------------------------------------------------------

/// Path parameters for repo-level collaborator routes with a target user ID.
#[derive(Debug, Deserialize)]
pub struct RepoCollabPath {
    pub org_id: Uuid,
    pub repo: String,
    pub user_id: Uuid,
}

/// Path parameters for listing collaborators (no user_id).
#[derive(Debug, Deserialize)]
pub struct RepoPath {
    pub org_id: Uuid,
    pub repo: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a repo from the `{org_id}/{repo}` path segments.
/// Returns the repo ID after verifying the caller has Admin permission.
async fn resolve_repo_for_admin(
    pool: &sqlx::PgPool,
    caller_id: Uuid,
    org_id: Uuid,
    repo_slug: &str,
) -> Result<Uuid, ApiError> {
    let repo = resolve_repo(pool, org_id, repo_slug).await?;

    // Enforce Admin permission (only owners can manage collaborators).
    service::check_repo_access(pool, Some(caller_id), repo.id, Permission::Admin).await?;

    Ok(repo.id)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /repos/{org_id}/{repo}/collaborators
///
/// List all collaborators of a repository. Requires Admin (owner) permission.
pub async fn list_collaborators(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<RepoPath>,
) -> Result<Json<Vec<RepoMember>>, ApiError> {
    let repo_id = resolve_repo_for_admin(&state.db, user.id, path.org_id, &path.repo).await?;

    let members = service::list_collaborators(&state.db, repo_id).await?;
    Ok(Json(members))
}

/// PUT /repos/{org_id}/{repo}/collaborators/{user_id}
///
/// Add or update a collaborator. Requires Admin (owner) permission.
/// The request body must contain a `role` field.
pub async fn add_or_update_collaborator(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<RepoCollabPath>,
    Json(body): Json<AddOrUpdateCollaboratorRequest>,
) -> Result<(StatusCode, Json<RepoMember>), ApiError> {
    let repo_id = resolve_repo_for_admin(&state.db, user.id, path.org_id, &path.repo).await?;

    let target_user_id = path.user_id;

    // Check if the target user already has a membership.
    let existing_role = service::get_user_role(&state.db, target_user_id, repo_id).await?;

    match existing_role {
        Some(_) => {
            // Update existing collaborator's role.
            let member =
                service::update_collaborator_role(&state.db, repo_id, target_user_id, body.role)
                    .await?;
            Ok((StatusCode::OK, Json(member)))
        }
        None => {
            // Add new collaborator.
            let member =
                service::add_collaborator(&state.db, repo_id, target_user_id, body.role).await?;
            Ok((StatusCode::CREATED, Json(member)))
        }
    }
}

/// DELETE /repos/{org_id}/{repo}/collaborators/{user_id}
///
/// Remove a collaborator. Requires Admin (owner) permission.
/// Cannot remove the repository owner.
pub async fn remove_collaborator(
    RequireAuth(user): RequireAuth,
    State(state): State<AppState>,
    Path(path): Path<RepoCollabPath>,
) -> Result<StatusCode, ApiError> {
    let repo_id = resolve_repo_for_admin(&state.db, user.id, path.org_id, &path.repo).await?;

    let target_user_id = path.user_id;

    service::remove_collaborator(&state.db, repo_id, target_user_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Router for collaborator management endpoints.
///
/// Mounts:
/// - `GET    /repos/{org_id}/{repo}/collaborators`
/// - `PUT    /repos/{org_id}/{repo}/collaborators/{user_id}`
/// - `DELETE /repos/{org_id}/{repo}/collaborators/{user_id}`
pub fn collaborator_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{org_id}/{repo}/collaborators",
            get(list_collaborators),
        )
        .route(
            "/repos/{org_id}/{repo}/collaborators/{user_id}",
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
        let result: Result<AddOrUpdateCollaboratorRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}

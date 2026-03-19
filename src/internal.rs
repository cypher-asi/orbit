use axum::{extract::State, http::StatusCode, Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::middleware::InternalAuth;
use crate::errors::ApiError;
use crate::repos::models::{CreateRepoInput, RepoResponse, Visibility};
use crate::repos::service;

/// Request body for `POST /internal/repos`.
///
/// Called by aura-network when a project is created to auto-create an orbit repo.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalCreateRepoRequest {
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
}

/// `POST /internal/repos` — Auto-create an orbit repo for an aura-network project.
///
/// Authenticated via `X-Internal-Token` header (service-to-service).
async fn create_repo(
    _auth: InternalAuth,
    State(state): State<AppState>,
    Json(input): Json<InternalCreateRepoRequest>,
) -> Result<(StatusCode, Json<RepoResponse>), ApiError> {
    let repo_input = CreateRepoInput {
        name: input.name,
        description: input.description,
        visibility: input.visibility,
    };

    let repo = service::create_repo(
        &state.db,
        &state.git_storage_root,
        input.owner_id,
        input.org_id,
        input.project_id,
        repo_input,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(RepoResponse::from(repo))))
}

/// Build the Router for internal endpoints.
///
/// Mounts:
/// - `POST /internal/repos` — auto-create repo (X-Internal-Token auth)
pub fn internal_routes() -> Router<AppState> {
    use axum::routing::post;
    Router::new().route("/internal/repos", post(create_repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_create_repo_request_deserializes() {
        let json = serde_json::json!({
            "orgId": "00000000-0000-0000-0000-000000000001",
            "projectId": "00000000-0000-0000-0000-000000000002",
            "ownerId": "00000000-0000-0000-0000-000000000003",
            "name": "my-project-repo"
        });
        let req: InternalCreateRepoRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name, "my-project-repo");
        assert!(req.visibility.is_none());
        assert!(req.description.is_none());
    }

    #[test]
    fn internal_create_repo_request_with_all_fields() {
        let json = serde_json::json!({
            "orgId": "00000000-0000-0000-0000-000000000001",
            "projectId": "00000000-0000-0000-0000-000000000002",
            "ownerId": "00000000-0000-0000-0000-000000000003",
            "name": "my-project-repo",
            "description": "Auto-created for project",
            "visibility": "public"
        });
        let req: InternalCreateRepoRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name, "my-project-repo");
        assert_eq!(req.description.as_deref(), Some("Auto-created for project"));
        assert_eq!(req.visibility, Some(Visibility::Public));
    }
}

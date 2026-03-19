use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::errors::ApiError;
use crate::users::routes::UserResponse;

use super::middleware::RequireAuth;
use super::service;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// JSON body for `POST /auth/login`.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Username or email address.
    pub login: String,
    /// Plain-text password.
    pub password: String,
}

/// JSON body for `POST /auth/tokens`.
#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    /// Human-readable name for the token.
    pub name: String,
    /// Optional expiry in days. `None` means the token never expires.
    pub expires_in_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Response for a successful login.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// The raw token (returned only once).
    pub token: String,
    /// The authenticated user.
    pub user: UserResponse,
}

/// Response for a newly created PAT.
#[derive(Debug, Serialize)]
pub struct CreateTokenResponse {
    /// The raw token (returned only once).
    pub token: String,
    /// Database ID of the token.
    pub id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// When the token expires, if at all.
    pub expires_at: Option<DateTime<Utc>>,
}

/// Info about an existing token (no hash or raw value).
#[derive(Debug, Serialize)]
pub struct TokenInfoResponse {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /auth/login`
///
/// Authenticate with username/email + password. Returns a session token and
/// user info on success. Returns a generic 401 on any failure to prevent
/// user enumeration.
async fn login_handler(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    // Delegate entirely to the auth service which returns a generic error
    // for unknown users, wrong passwords, and disabled accounts alike.
    let (raw_token, auth_token) = service::login(&state.db, &body.login, &body.password).await?;

    // Fetch the user for the response.
    let user = crate::users::service::get_user_by_id(&state.db, auth_token.user_id)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("invalid credentials".to_string()))?;

    Ok(Json(LoginResponse {
        token: raw_token,
        user: UserResponse::from(user),
    }))
}

/// `POST /auth/tokens`
///
/// Create a new personal access token. Requires authentication.
async fn create_token_handler(
    RequireAuth(authed): RequireAuth,
    State(state): State<AppState>,
    Json(body): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), ApiError> {
    if body.name.is_empty() {
        return Err(ApiError::BadRequest("token name must not be empty".to_string()));
    }

    let expires_in = body.expires_in_days.map(|days| {
        std::time::Duration::from_secs(u64::from(days) * 24 * 60 * 60)
    });

    let (raw_token, auth_token) =
        service::create_token(&state.db, authed.id, &body.name, expires_in).await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            token: raw_token,
            id: auth_token.id,
            name: auth_token.name,
            expires_at: auth_token.expires_at,
        }),
    ))
}

/// `GET /auth/tokens`
///
/// List all non-revoked tokens belonging to the authenticated user.
async fn list_tokens_handler(
    RequireAuth(authed): RequireAuth,
    State(state): State<AppState>,
) -> Result<Json<Vec<TokenInfoResponse>>, ApiError> {
    let tokens = service::list_tokens(&state.db, authed.id).await?;

    let response: Vec<TokenInfoResponse> = tokens
        .into_iter()
        .map(|t| TokenInfoResponse {
            id: t.id,
            name: t.name,
            created_at: t.created_at,
            expires_at: t.expires_at,
        })
        .collect();

    Ok(Json(response))
}

/// `DELETE /auth/tokens/{id}`
///
/// Revoke a token. The token must belong to the authenticated user.
async fn revoke_token_handler(
    RequireAuth(authed): RequireAuth,
    State(state): State<AppState>,
    Path(token_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    service::revoke_token(&state.db, token_id, authed.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the `Router` for authentication endpoints (token listing and revocation).
///
/// Mounts:
/// - `GET    /auth/tokens`
/// - `DELETE /auth/tokens/{id}`
///
/// These are read/delete operations that require authentication but do not
/// need aggressive rate limiting.
pub fn auth_token_read_routes() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route("/auth/tokens", get(list_tokens_handler))
        .route("/auth/tokens/{id}", delete(revoke_token_handler))
}

/// Build a `Router` containing only the login endpoint.
///
/// This is separated from `auth_routes()` so the central router can apply
/// rate limiting selectively to the login endpoint without affecting the
/// authenticated PAT management routes.
pub fn auth_login_routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login_handler))
}

/// Return a function reference for the create-token handler.
///
/// This allows the central router to mount it on its own route with a
/// separate rate-limit layer (20/min per IP for token creation).
pub fn create_token_handler_fn() -> axum::routing::MethodRouter<AppState> {
    post(create_token_handler)
}

/// Build the `Router` for all authentication endpoints (convenience).
///
/// Mounts:
/// - `POST   /auth/login`
/// - `POST   /auth/tokens`
/// - `GET    /auth/tokens`
/// - `DELETE /auth/tokens/{id}`
///
/// Note: The central router typically uses the individual route builders
/// (`auth_login_routes`, `auth_token_read_routes`, `create_token_handler_fn`)
/// to apply different rate limits. This combined router is provided for
/// convenience in contexts where unified mounting is preferred.
pub fn auth_routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login_handler))
        .route("/auth/tokens", post(create_token_handler).get(list_tokens_handler))
        .route("/auth/tokens/{id}", delete(revoke_token_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_request_deserializes() {
        let json = serde_json::json!({
            "login": "alice",
            "password": "secret123"
        });
        let req: LoginRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.login, "alice");
        assert_eq!(req.password, "secret123");
    }

    #[test]
    fn login_request_with_email() {
        let json = serde_json::json!({
            "login": "alice@example.com",
            "password": "secret123"
        });
        let req: LoginRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.login, "alice@example.com");
    }

    #[test]
    fn create_token_request_deserializes_with_expiry() {
        let json = serde_json::json!({
            "name": "ci-token",
            "expires_in_days": 30
        });
        let req: CreateTokenRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name, "ci-token");
        assert_eq!(req.expires_in_days, Some(30));
    }

    #[test]
    fn create_token_request_deserializes_without_expiry() {
        let json = serde_json::json!({
            "name": "permanent-token"
        });
        let req: CreateTokenRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name, "permanent-token");
        assert_eq!(req.expires_in_days, None);
    }

    #[test]
    fn login_response_serializes() {
        let resp = LoginResponse {
            token: "abc123".to_string(),
            user: UserResponse {
                id: Uuid::nil(),
                username: "alice".to_string(),
                email: "alice@example.com".to_string(),
                display_name: None,
                is_admin: false,
                is_disabled: false,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["token"], "abc123");
        assert_eq!(json["user"]["username"], "alice");
        // Ensure no password_hash leaks
        assert!(json["user"].get("password_hash").is_none());
    }

    #[test]
    fn create_token_response_serializes() {
        let resp = CreateTokenResponse {
            token: "raw-token".to_string(),
            id: Uuid::nil(),
            name: "my-token".to_string(),
            expires_at: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["token"], "raw-token");
        assert_eq!(json["name"], "my-token");
        assert!(json["expires_at"].is_null());
    }

    #[test]
    fn token_info_response_serializes() {
        let resp = TokenInfoResponse {
            id: Uuid::nil(),
            name: "test".to_string(),
            created_at: chrono::Utc::now(),
            expires_at: Some(chrono::Utc::now()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["name"], "test");
        // No token hash or raw value
        assert!(json.get("token_hash").is_none());
        assert!(json.get("token").is_none());
    }
}

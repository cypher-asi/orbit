use axum::{extract::FromRequestParts, http::request::Parts};

use crate::app_state::AppState;
use crate::errors::ApiError;
use crate::users::models::User;

/// Extractor that resolves the authenticated user from a Bearer token.
///
/// Reads the `Authorization: Bearer <token>` header, looks up the token
/// in the `auth_tokens` table, and loads the associated user.
///
/// Returns `ApiError::Unauthorized` if the token is missing, invalid,
/// expired, or revoked, or if the user is disabled.
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Extract Authorization header
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized("missing authorization header".to_string()))?;

        // Must be Bearer token
        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::Unauthorized("invalid authorization scheme".to_string()))?;

        if token.is_empty() {
            return Err(ApiError::Unauthorized("missing token".to_string()));
        }

        // Hash the token for lookup (tokens are stored hashed using SHA-256)
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = format!("{:x}", hasher.finalize());

        // Look up the token in the database
        let row = sqlx::query_as::<_, TokenRow>(
            r#"
            SELECT t.user_id, t.expires_at, t.revoked_at
            FROM auth_tokens t
            WHERE t.token_hash = $1
            "#,
        )
        .bind(&token_hash)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to look up auth token");
            ApiError::Internal("authentication error".to_string())
        })?
        .ok_or_else(|| ApiError::Unauthorized("invalid token".to_string()))?;

        // Check if token is revoked
        if row.revoked_at.is_some() {
            return Err(ApiError::Unauthorized("token has been revoked".to_string()));
        }

        // Check if token is expired
        if let Some(expires_at) = row.expires_at {
            if expires_at < chrono::Utc::now() {
                return Err(ApiError::Unauthorized("token has expired".to_string()));
            }
        }

        // Load the user
        let user = crate::users::service::get_user_by_id(&state.db, row.user_id)
            .await?
            .ok_or_else(|| ApiError::Unauthorized("user not found".to_string()))?;

        // Check if user is disabled
        if user.is_disabled {
            return Err(ApiError::Unauthorized("account is disabled".to_string()));
        }

        Ok(AuthUser(user))
    }
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    user_id: uuid::Uuid,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

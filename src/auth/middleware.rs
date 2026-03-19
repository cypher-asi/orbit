use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::errors::ApiError;

/// Lightweight struct carrying the essential fields of an authenticated user.
///
/// Downstream handlers receive this instead of the full `User` model so that
/// sensitive fields (password hash, timestamps, etc.) are never accidentally
/// leaked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticatedUser {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub is_admin: bool,
}

/// Extract a raw token from the `Authorization` header.
///
/// Supports two schemes:
/// - `Bearer <token>` -- the token is used directly.
/// - `Basic <base64>` -- the base64 payload is decoded to `username:token`;
///   only the token (password) portion is used, for Git HTTP compatibility.
///
/// Returns `None` if there is no `Authorization` header.
/// Returns `Err` if the header is present but malformed.
fn extract_raw_token(parts: &Parts) -> Result<Option<String>, ApiError> {
    let header_value = match parts.headers.get("authorization") {
        Some(v) => v,
        None => return Ok(None),
    };

    let header_str = header_value
        .to_str()
        .map_err(|_| ApiError::Unauthorized("invalid authorization header".to_string()))?;

    if let Some(token) = header_str.strip_prefix("Bearer ") {
        let token = token.trim();
        if token.is_empty() {
            return Err(ApiError::Unauthorized("missing token".to_string()));
        }
        return Ok(Some(token.to_string()));
    }

    if let Some(encoded) = header_str.strip_prefix("Basic ") {
        let encoded = encoded.trim();
        let decoded_bytes = STANDARD
            .decode(encoded)
            .map_err(|_| ApiError::Unauthorized("invalid basic auth encoding".to_string()))?;
        let decoded = String::from_utf8(decoded_bytes)
            .map_err(|_| ApiError::Unauthorized("invalid basic auth encoding".to_string()))?;

        // Format: username:token -- we only care about the token part.
        let token_part = match decoded.split_once(':') {
            Some((_username, token)) => token,
            None => {
                return Err(ApiError::Unauthorized(
                    "invalid basic auth format".to_string(),
                ));
            }
        };

        if token_part.is_empty() {
            return Err(ApiError::Unauthorized("missing token".to_string()));
        }

        return Ok(Some(token_part.to_string()));
    }

    Err(ApiError::Unauthorized(
        "unsupported authorization scheme".to_string(),
    ))
}

/// Verify the raw token against the database and return an `AuthenticatedUser`
/// if valid.
async fn resolve_user(
    state: &AppState,
    raw_token: &str,
) -> Result<AuthenticatedUser, ApiError> {
    let user = super::service::verify_token(&state.db, raw_token)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("invalid or expired token".to_string()))?;

    Ok(AuthenticatedUser {
        id: user.id,
        username: user.username,
        email: user.email,
        is_admin: user.is_admin,
    })
}

// ---------------------------------------------------------------------------
// RequireAuth extractor
// ---------------------------------------------------------------------------

/// Axum extractor that **requires** a valid authentication token.
///
/// Reads the `Authorization` header (Bearer or Basic), verifies the token,
/// and resolves to an `AuthenticatedUser`. Returns `401 Unauthorized` if the
/// token is missing, invalid, expired, revoked, or the user is disabled.
///
/// # Example
/// ```ignore
/// async fn handler(RequireAuth(user): RequireAuth) -> impl IntoResponse {
///     format!("Hello, {}!", user.username)
/// }
/// ```
pub struct RequireAuth(pub AuthenticatedUser);

impl FromRequestParts<AppState> for RequireAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let raw_token = extract_raw_token(parts)?
            .ok_or_else(|| {
                ApiError::Unauthorized("missing authorization header".to_string())
            })?;

        let user = resolve_user(state, &raw_token).await?;
        Ok(RequireAuth(user))
    }
}

// ---------------------------------------------------------------------------
// OptionalAuth extractor
// ---------------------------------------------------------------------------

/// Axum extractor that **optionally** resolves an authenticated user.
///
/// If no `Authorization` header is present the inner value is `None`,
/// allowing unauthenticated access (useful for public repository reads).
/// If a header *is* present but the token is invalid, this still returns
/// `401 Unauthorized` so that callers with bad credentials are not silently
/// treated as anonymous.
///
/// # Example
/// ```ignore
/// async fn handler(OptionalAuth(user): OptionalAuth) -> impl IntoResponse {
///     match user {
///         Some(u) => format!("Hello, {}!", u.username),
///         None => "Hello, anonymous!".to_string(),
///     }
/// }
/// ```
pub struct OptionalAuth(pub Option<AuthenticatedUser>);

impl FromRequestParts<AppState> for OptionalAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let raw_token = match extract_raw_token(parts)? {
            Some(t) => t,
            None => return Ok(OptionalAuth(None)),
        };

        let user = resolve_user(state, &raw_token).await?;
        Ok(OptionalAuth(Some(user)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parts(auth_header: Option<&str>) -> axum::http::request::Parts {
        let mut builder = axum::http::Request::builder();
        if let Some(val) = auth_header {
            builder = builder.header("authorization", val);
        }
        builder.body(()).unwrap().into_parts().0
    }

    #[test]
    fn extract_bearer_token() {
        let parts = make_parts(Some("Bearer my-secret-token"));
        let token = extract_raw_token(&parts).unwrap();
        assert_eq!(token, Some("my-secret-token".to_string()));
    }

    #[test]
    fn extract_basic_token() {
        // "user:the-pat" base64 encoded
        let encoded = STANDARD.encode("user:the-pat");
        let header_val = format!("Basic {}", encoded);
        let parts = make_parts(Some(&header_val));
        let token = extract_raw_token(&parts).unwrap();
        assert_eq!(token, Some("the-pat".to_string()));
    }

    #[test]
    fn extract_no_header_returns_none() {
        let parts = make_parts(None);
        let token = extract_raw_token(&parts).unwrap();
        assert!(token.is_none());
    }

    #[test]
    fn extract_empty_bearer_is_error() {
        let parts = make_parts(Some("Bearer "));
        let result = extract_raw_token(&parts);
        assert!(result.is_err());
    }

    #[test]
    fn extract_basic_missing_colon_is_error() {
        let encoded = STANDARD.encode("no-colon-here");
        let header_val = format!("Basic {}", encoded);
        let parts = make_parts(Some(&header_val));
        let result = extract_raw_token(&parts);
        assert!(result.is_err());
    }

    #[test]
    fn extract_basic_empty_password_is_error() {
        let encoded = STANDARD.encode("user:");
        let header_val = format!("Basic {}", encoded);
        let parts = make_parts(Some(&header_val));
        let result = extract_raw_token(&parts);
        assert!(result.is_err());
    }

    #[test]
    fn extract_unsupported_scheme_is_error() {
        let parts = make_parts(Some("Digest abc123"));
        let result = extract_raw_token(&parts);
        assert!(result.is_err());
    }

    #[test]
    fn authenticated_user_is_serializable() {
        let user = AuthenticatedUser {
            id: Uuid::nil(),
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            is_admin: false,
        };
        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("alice"));
    }
}

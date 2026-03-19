use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::errors::ApiError;

/// Lightweight struct carrying the authenticated user's identity.
///
/// With zOS JWT auth, only the user UUID is available from the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatedUser {
    pub id: Uuid,
}

/// Extract a raw token from the `Authorization` header.
///
/// Supports two schemes:
/// - `Bearer <token>` -- the token is used directly (API calls).
/// - `Basic <base64>` -- the base64 payload is decoded to `username:token`;
///   only the token (password) portion is used. For Git HTTP, the JWT is
///   passed as the password.
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

        // Format: username:token -- the token (password) is the JWT.
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

/// Validate a JWT token and return an `AuthenticatedUser`.
async fn resolve_user(state: &AppState, raw_token: &str) -> Result<AuthenticatedUser, ApiError> {
    let claims = state
        .token_validator
        .validate(raw_token)
        .await
        .map_err(ApiError::Unauthorized)?;

    let user_id_str = claims
        .user_id()
        .ok_or_else(|| ApiError::Unauthorized("token missing user ID".to_string()))?;

    let id = user_id_str
        .parse::<Uuid>()
        .map_err(|_| ApiError::Unauthorized("invalid user ID in token".to_string()))?;

    Ok(AuthenticatedUser { id })
}

// ---------------------------------------------------------------------------
// RequireAuth extractor
// ---------------------------------------------------------------------------

/// Axum extractor that **requires** a valid JWT token.
///
/// Reads the `Authorization` header (Bearer or Basic), validates the JWT,
/// and resolves to an `AuthenticatedUser`. Returns `401 Unauthorized` if the
/// token is missing, invalid, or the user ID cannot be extracted.
pub struct RequireAuth(pub AuthenticatedUser);

impl FromRequestParts<AppState> for RequireAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let raw_token = extract_raw_token(parts)?
            .ok_or_else(|| ApiError::Unauthorized("missing authorization header".to_string()))?;

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
/// `401 Unauthorized`.
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

// ---------------------------------------------------------------------------
// InternalAuth extractor
// ---------------------------------------------------------------------------

/// Axum extractor for service-to-service auth via `X-Internal-Token` header.
pub struct InternalAuth;

impl FromRequestParts<AppState> for InternalAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get("x-internal-token")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized("missing internal token".to_string()))?;

        if token != state.config.internal_service_token {
            return Err(ApiError::Unauthorized("invalid internal token".to_string()));
        }

        Ok(InternalAuth)
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
        let encoded = STANDARD.encode("user:the-jwt");
        let header_val = format!("Basic {}", encoded);
        let parts = make_parts(Some(&header_val));
        let token = extract_raw_token(&parts).unwrap();
        assert_eq!(token, Some("the-jwt".to_string()));
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
}

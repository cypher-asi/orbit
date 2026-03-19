use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;
use crate::users::models::User;

use super::models::{AuthToken, AuthTokenInfo};
use super::password::verify_password;
use super::token::{generate_token, hash_token};

/// Authenticate a user by username or email and password.
///
/// On success, creates a new token and returns the raw token string along
/// with the persisted `AuthToken` record. The raw token is the only time
/// the plaintext value is available -- it is stored as a SHA-256 hash.
///
/// Returns a generic `Unauthorized` error on any failure to avoid user
/// enumeration.
pub async fn login(
    pool: &PgPool,
    username_or_email: &str,
    password: &str,
) -> Result<(String, AuthToken), ApiError> {
    let generic_err = || ApiError::Unauthorized("invalid credentials".to_string());

    // Look up by username first, then by email.
    let user: User =
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = $1 OR email = $1 LIMIT 1")
            .bind(username_or_email)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "login: failed to query user");
                generic_err()
            })?
            .ok_or_else(generic_err)?;

    // Reject disabled accounts.
    if user.is_disabled {
        return Err(generic_err());
    }

    // Verify password.
    let valid = verify_password(password, &user.password_hash)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "login: password verification error");
            generic_err()
        })?;

    if !valid {
        return Err(generic_err());
    }

    // Create a session token (named "login-session").
    let (raw_token, auth_token) = create_token(pool, user.id, "login-session", None).await?;

    Ok((raw_token, auth_token))
}

/// Create a new personal access token for the given user.
///
/// Returns a tuple of the raw token (to show the user once) and the
/// persisted `AuthToken` row.
pub async fn create_token(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    expires_in: Option<std::time::Duration>,
) -> Result<(String, AuthToken), ApiError> {
    let (raw_token, token_hash) = generate_token();

    let expires_at = expires_in
        .map(|d| Utc::now() + Duration::from_std(d).unwrap_or_else(|_| Duration::seconds(0)));

    let auth_token = sqlx::query_as::<_, AuthToken>(
        r#"
        INSERT INTO auth_tokens (user_id, token_hash, name, expires_at)
        VALUES ($1, $2, $3, $4)
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(&token_hash)
    .bind(name)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "create_token: failed to insert auth token");
        ApiError::Internal("failed to create token".to_string())
    })?;

    Ok((raw_token, auth_token))
}

/// Revoke a token by setting its `revoked_at` timestamp.
///
/// Only the token owner may revoke their own tokens. Returns
/// `ApiError::NotFound` if the token does not exist or does not belong to
/// the specified user.
pub async fn revoke_token(pool: &PgPool, token_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE auth_tokens
        SET revoked_at = now()
        WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL
        "#,
    )
    .bind(token_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "revoke_token: failed to update auth token");
        ApiError::Internal("failed to revoke token".to_string())
    })?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("token not found".to_string()));
    }

    Ok(())
}

/// List all non-revoked tokens for a user.
///
/// Returns non-sensitive token metadata (no hash).
pub async fn list_tokens(pool: &PgPool, user_id: Uuid) -> Result<Vec<AuthTokenInfo>, ApiError> {
    let tokens = sqlx::query_as::<_, AuthTokenInfo>(
        r#"
        SELECT id, name, created_at, expires_at
        FROM auth_tokens
        WHERE user_id = $1 AND revoked_at IS NULL
        ORDER BY created_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "list_tokens: failed to list auth tokens");
        ApiError::Internal("failed to list tokens".to_string())
    })?;

    Ok(tokens)
}

/// Verify a raw token and return the associated user if valid.
///
/// Hashes the raw token, looks it up in the database, checks that it is
/// not revoked or expired, and then loads and returns the user. Disabled
/// users are rejected.
///
/// Returns `Ok(None)` when the token is not found (rather than an error)
/// to allow callers to decide how to handle unauthenticated requests.
pub async fn verify_token(pool: &PgPool, raw_token: &str) -> Result<Option<User>, ApiError> {
    let token_hash_value = hash_token(raw_token);

    // Single query: join auth_tokens with users, filter valid tokens.
    let user = sqlx::query_as::<_, User>(
        r#"
        SELECT u.*
        FROM auth_tokens t
        JOIN users u ON u.id = t.user_id
        WHERE t.token_hash = $1
          AND t.revoked_at IS NULL
          AND (t.expires_at IS NULL OR t.expires_at > now())
          AND u.is_disabled = false
        "#,
    )
    .bind(&token_hash_value)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "verify_token: failed to look up token");
        ApiError::Internal("authentication error".to_string())
    })?;

    Ok(user)
}

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordVerifier, SaltString},
    Argon2, PasswordHasher,
};

use crate::errors::ApiError;

/// Hash a plaintext password using Argon2id with a random salt.
///
/// Returns the PHC-formatted hash string suitable for storage.
/// Uses `spawn_blocking` to avoid blocking the async runtime during
/// the CPU-intensive hashing operation.
pub async fn hash_password(password: &str) -> Result<String, ApiError> {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| {
                tracing::error!(error = %e, "failed to hash password");
                ApiError::Internal("failed to hash password".to_string())
            })?;
        Ok(hash.to_string())
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "spawn_blocking panicked during password hashing");
        ApiError::Internal("failed to hash password".to_string())
    })?
}

/// Verify a plaintext password against an Argon2 PHC-formatted hash.
///
/// Returns `true` if the password matches, `false` otherwise.
/// Returns an error only on internal/parsing failures.
/// Uses `spawn_blocking` to avoid blocking the async runtime during
/// the CPU-intensive verification operation.
pub async fn verify_password(password: &str, password_hash: &str) -> Result<bool, ApiError> {
    let password = password.to_string();
    let password_hash = password_hash.to_string();
    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&password_hash).map_err(|e| {
            tracing::error!(error = %e, "failed to parse password hash");
            ApiError::Internal("failed to verify password".to_string())
        })?;
        let argon2 = Argon2::default();
        Ok(argon2
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "spawn_blocking panicked during password verification");
        ApiError::Internal("failed to verify password".to_string())
    })?
}

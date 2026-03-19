use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::auth::{hash_password, AuthUser};
use crate::errors::ApiError;
use crate::users::models::{CreateUserInput, UpdateUserInput, User};
use crate::users::service;

// ---------------------------------------------------------------------------
// Response types (exclude password_hash)
// ---------------------------------------------------------------------------

/// JSON response for a user, omitting the password_hash field.
#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub display_name: Option<String>,
    pub is_admin: bool,
    pub is_disabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<User> for UserResponse {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            username: u.username,
            email: u.email,
            display_name: u.display_name,
            is_admin: u.is_admin,
            is_disabled: u.is_disabled,
            created_at: u.created_at,
            updated_at: u.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// JSON body for POST /auth/register
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    pub password: String,
    pub display_name: Option<String>,
}

/// JSON body for PATCH /users/me
#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub display_name: Option<String>,
    pub email: Option<String>,
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate a username: 1-64 chars, alphanumeric + hyphens, starts with a letter.
fn validate_username(username: &str) -> Result<(), ApiError> {
    if username.is_empty() || username.len() > 64 {
        return Err(ApiError::BadRequest(
            "username must be between 1 and 64 characters".to_string(),
        ));
    }

    let first = username.chars().next().unwrap();
    if !first.is_ascii_alphabetic() {
        return Err(ApiError::BadRequest(
            "username must start with a letter".to_string(),
        ));
    }

    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(ApiError::BadRequest(
            "username must contain only alphanumeric characters and hyphens".to_string(),
        ));
    }

    Ok(())
}

/// Validate an email address: basic format check and max 255 chars.
fn validate_email(email: &str) -> Result<(), ApiError> {
    if email.is_empty() || email.len() > 255 {
        return Err(ApiError::BadRequest(
            "email must be between 1 and 255 characters".to_string(),
        ));
    }

    // Basic email format: must contain exactly one '@' with non-empty local and domain parts,
    // and the domain must contain at least one '.'.
    let at_pos = email.find('@');
    match at_pos {
        Some(pos) => {
            let local = &email[..pos];
            let domain = &email[pos + 1..];
            if local.is_empty() || domain.is_empty() || !domain.contains('.') {
                return Err(ApiError::BadRequest(
                    "invalid email format".to_string(),
                ));
            }
            // Make sure there's only one '@'
            if email.chars().filter(|&c| c == '@').count() != 1 {
                return Err(ApiError::BadRequest(
                    "invalid email format".to_string(),
                ));
            }
        }
        None => {
            return Err(ApiError::BadRequest(
                "invalid email format".to_string(),
            ));
        }
    }

    Ok(())
}

/// Validate a password: minimum 8 characters.
fn validate_password(password: &str) -> Result<(), ApiError> {
    if password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".to_string(),
        ));
    }
    Ok(())
}

/// Validate a display name: max 128 chars.
fn validate_display_name(display_name: &str) -> Result<(), ApiError> {
    if display_name.len() > 128 {
        return Err(ApiError::BadRequest(
            "display name must be at most 128 characters".to_string(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /auth/register - Register a new user
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<UserResponse>), ApiError> {
    // Validate input
    validate_username(&body.username)?;
    validate_email(&body.email)?;
    validate_password(&body.password)?;
    if let Some(ref dn) = body.display_name {
        validate_display_name(dn)?;
    }

    // Hash the password
    let password_hash = hash_password(&body.password).await?;

    // Create the user
    let input = CreateUserInput {
        username: body.username,
        email: body.email,
        password_hash,
        display_name: body.display_name,
    };

    let user = service::create_user(&state.db, input).await?;

    Ok((StatusCode::CREATED, Json(UserResponse::from(user))))
}

/// GET /users/me - Get current user profile
pub async fn get_me(
    AuthUser(user): AuthUser,
) -> Result<Json<UserResponse>, ApiError> {
    Ok(Json(UserResponse::from(user)))
}

/// PATCH /users/me - Update current user profile
pub async fn update_me(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<UserResponse>, ApiError> {
    // Validate fields if provided
    if let Some(ref email) = body.email {
        validate_email(email)?;
    }
    if let Some(ref dn) = body.display_name {
        validate_display_name(dn)?;
    }

    let input = UpdateUserInput {
        display_name: body.display_name,
        email: body.email,
    };

    let updated = service::update_user(&state.db, user.id, input).await?;

    Ok(Json(UserResponse::from(updated)))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Router for user-related endpoints.
///
/// Mounts:
/// - `POST /auth/register`
/// - `GET  /users/me`
/// - `PATCH /users/me`
pub fn users_routes() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/users/me", get(get_me).patch(update_me))
}

/// Build a Router containing only user profile routes.
///
/// Mounts:
/// - `GET  /users/me`
/// - `PATCH /users/me`
///
/// This is used by the central router to separate rate-limited auth routes
/// from non-rate-limited profile routes.
pub fn users_profile_routes() -> Router<AppState> {
    Router::new()
        .route("/users/me", get(get_me).patch(update_me))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_username_valid() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("a").is_ok());
        assert!(validate_username("alice-bob").is_ok());
        assert!(validate_username("A123").is_ok());
        assert!(validate_username("a-b-c").is_ok());
    }

    #[test]
    fn test_validate_username_invalid() {
        // Empty
        assert!(validate_username("").is_err());
        // Starts with digit
        assert!(validate_username("1abc").is_err());
        // Starts with hyphen
        assert!(validate_username("-abc").is_err());
        // Contains underscore
        assert!(validate_username("abc_def").is_err());
        // Contains space
        assert!(validate_username("abc def").is_err());
        // Too long (65 chars)
        let long = format!("a{}", "b".repeat(64));
        assert!(validate_username(&long).is_err());
    }

    #[test]
    fn test_validate_email_valid() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("a@b.c").is_ok());
        assert!(validate_email("user+tag@domain.co.uk").is_ok());
    }

    #[test]
    fn test_validate_email_invalid() {
        assert!(validate_email("").is_err());
        assert!(validate_email("noatsign").is_err());
        assert!(validate_email("@domain.com").is_err());
        assert!(validate_email("user@").is_err());
        assert!(validate_email("user@domain").is_err());
        // Over 255 chars
        let long = format!("a@{}.com", "b".repeat(250));
        assert!(validate_email(&long).is_err());
    }

    #[test]
    fn test_validate_password_valid() {
        assert!(validate_password("12345678").is_ok());
        assert!(validate_password("a very long password").is_ok());
    }

    #[test]
    fn test_validate_password_invalid() {
        assert!(validate_password("").is_err());
        assert!(validate_password("1234567").is_err());
    }

    #[test]
    fn test_validate_display_name_valid() {
        assert!(validate_display_name("Alice").is_ok());
        assert!(validate_display_name("").is_ok()); // empty is ok, it's optional
        assert!(validate_display_name(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn test_validate_display_name_invalid() {
        assert!(validate_display_name(&"a".repeat(129)).is_err());
    }

    #[test]
    fn test_user_response_from_user() {
        let user = User {
            id: Uuid::new_v4(),
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            password_hash: "secret_hash".to_string(),
            display_name: Some("Alice".to_string()),
            is_admin: false,
            is_disabled: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let resp = UserResponse::from(user.clone());
        assert_eq!(resp.id, user.id);
        assert_eq!(resp.username, "alice");
        assert_eq!(resp.email, "alice@example.com");
        assert_eq!(resp.display_name, Some("Alice".to_string()));
        assert!(!resp.is_admin);
        assert!(!resp.is_disabled);

        // Ensure password_hash is not in the serialized JSON
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("password_hash").is_none());
    }
}

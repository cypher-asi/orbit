use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a row in the `users` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub display_name: Option<String>,
    pub is_admin: bool,
    pub is_disabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new user.
#[derive(Debug, Clone)]
pub struct CreateUserInput {
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub display_name: Option<String>,
}

/// Input for updating an existing user.
#[derive(Debug, Clone, Default)]
pub struct UpdateUserInput {
    pub display_name: Option<String>,
    pub email: Option<String>,
}

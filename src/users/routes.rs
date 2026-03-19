use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::users::models::User;

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

#[cfg(test)]
mod tests {
    use super::*;

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

        // Ensure password_hash is not in the serialized JSON
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("password_hash").is_none());
    }
}

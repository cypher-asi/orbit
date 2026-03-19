use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Repository-level roles.
///
/// Stored in the `repo_members.role` column as lowercase strings
/// ('owner', 'writer', 'reader').
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Writer,
    Reader,
}

impl Role {
    /// Convert the role to the string stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Writer => "writer",
            Role::Reader => "reader",
        }
    }

    /// Parse a role from a database string value.
    pub fn from_db_str(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "writer" => Some(Role::Writer),
            "reader" => Some(Role::Reader),
            _ => None,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// sqlx encoding/decoding: store as TEXT matching the DB varchar column.
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Role {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Role::from_db_str(s).ok_or_else(|| format!("unknown role: {}", s).into())
    }
}

impl sqlx::Type<sqlx::Postgres> for Role {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <&str as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <&str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for Role {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.as_str(), buf)
    }
}

/// Permission levels required for operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Read-level access: view, clone, fetch.
    Read,
    /// Write-level access: push, create/delete branches, open/merge PRs.
    Write,
    /// Admin-level access: manage collaborators, settings, visibility, archive/delete.
    Admin,
}

impl Role {
    /// Check whether this role satisfies the given permission requirement.
    pub fn has_permission(&self, required: Permission) -> bool {
        match required {
            Permission::Read => true, // all roles can read
            Permission::Write => matches!(self, Role::Owner | Role::Writer),
            Permission::Admin => matches!(self, Role::Owner),
        }
    }
}

/// A row from the `repo_members` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RepoMember {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub user_id: Uuid,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}

/// Minimal repo row used internally by permission checks (visibility and archived only).
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct RepoAccessRow {
    pub visibility: String,
    pub archived: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_as_str_round_trip() {
        for role in &[Role::Owner, Role::Writer, Role::Reader] {
            let s = role.as_str();
            let parsed = Role::from_db_str(s).unwrap();
            assert_eq!(*role, parsed);
        }
    }

    #[test]
    fn role_from_db_str_unknown() {
        assert!(Role::from_db_str("superadmin").is_none());
    }

    #[test]
    fn role_display() {
        assert_eq!(Role::Owner.to_string(), "owner");
        assert_eq!(Role::Writer.to_string(), "writer");
        assert_eq!(Role::Reader.to_string(), "reader");
    }

    #[test]
    fn role_serde_serialize() {
        let json = serde_json::to_string(&Role::Owner).unwrap();
        assert_eq!(json, r#""owner""#);
        let json = serde_json::to_string(&Role::Writer).unwrap();
        assert_eq!(json, r#""writer""#);
        let json = serde_json::to_string(&Role::Reader).unwrap();
        assert_eq!(json, r#""reader""#);
    }

    #[test]
    fn role_serde_deserialize() {
        let role: Role = serde_json::from_str(r#""owner""#).unwrap();
        assert_eq!(role, Role::Owner);
        let role: Role = serde_json::from_str(r#""writer""#).unwrap();
        assert_eq!(role, Role::Writer);
        let role: Role = serde_json::from_str(r#""reader""#).unwrap();
        assert_eq!(role, Role::Reader);
    }

    #[test]
    fn permission_serde() {
        let json = serde_json::to_string(&Permission::Read).unwrap();
        assert_eq!(json, r#""read""#);
        let p: Permission = serde_json::from_str(r#""write""#).unwrap();
        assert_eq!(p, Permission::Write);
        let p: Permission = serde_json::from_str(r#""admin""#).unwrap();
        assert_eq!(p, Permission::Admin);
    }

    #[test]
    fn owner_has_all_permissions() {
        assert!(Role::Owner.has_permission(Permission::Read));
        assert!(Role::Owner.has_permission(Permission::Write));
        assert!(Role::Owner.has_permission(Permission::Admin));
    }

    #[test]
    fn writer_has_read_and_write() {
        assert!(Role::Writer.has_permission(Permission::Read));
        assert!(Role::Writer.has_permission(Permission::Write));
        assert!(!Role::Writer.has_permission(Permission::Admin));
    }

    #[test]
    fn reader_has_read_only() {
        assert!(Role::Reader.has_permission(Permission::Read));
        assert!(!Role::Reader.has_permission(Permission::Write));
        assert!(!Role::Reader.has_permission(Permission::Admin));
    }

    #[test]
    fn repo_member_serde() {
        let member = RepoMember {
            id: uuid::Uuid::nil(),
            repo_id: uuid::Uuid::nil(),
            user_id: uuid::Uuid::nil(),
            role: Role::Writer,
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&member).unwrap();
        assert_eq!(json["role"], "writer");
        // Verify camelCase field names
        assert!(json.get("repoId").is_some());
        assert!(json.get("userId").is_some());
        assert!(json.get("createdAt").is_some());
    }
}

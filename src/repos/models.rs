use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Visibility enum
// ---------------------------------------------------------------------------

/// Repository visibility: public or private.
///
/// Stored in the `repos.visibility` column as a lowercase string
/// (`"public"` or `"private"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    /// Database string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
        }
    }

    /// Parse from a database string value.
    pub fn from_db_str(s: &str) -> Option<Visibility> {
        match s {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            _ => None,
        }
    }
}

impl Default for Visibility {
    fn default() -> Self {
        Visibility::Private
    }
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// sqlx encoding/decoding: store as TEXT matching the DB varchar column.
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Visibility {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Visibility::from_db_str(s)
            .ok_or_else(|| format!("unknown visibility: {}", s).into())
    }
}

impl sqlx::Type<sqlx::Postgres> for Visibility {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <&str as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <&str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for Visibility {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.as_str(), buf)
    }
}

// ---------------------------------------------------------------------------
// Repo (database row)
// ---------------------------------------------------------------------------

/// Represents a row in the `repos` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Repo {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub visibility: Visibility,
    pub default_branch: String,
    pub archived: bool,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// RepoResponse (API response, excludes deleted_at)
// ---------------------------------------------------------------------------

/// API response type for a repository. Excludes `deleted_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoResponse {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub visibility: Visibility,
    pub default_branch: String,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Repo> for RepoResponse {
    fn from(repo: Repo) -> Self {
        RepoResponse {
            id: repo.id,
            owner_id: repo.owner_id,
            name: repo.name,
            slug: repo.slug,
            description: repo.description,
            visibility: repo.visibility,
            default_branch: repo.default_branch,
            archived: repo.archived,
            created_at: repo.created_at,
            updated_at: repo.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// Input for creating a new repository.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateRepoInput {
    /// Repository name (used to derive the slug).
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Visibility; defaults to `Private` if omitted.
    #[serde(default)]
    pub visibility: Option<Visibility>,
}

/// Input for updating an existing repository.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRepoInput {
    /// New name (slug will be re-derived).
    pub name: Option<String>,
    /// New description.
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// Simple limit/offset pagination.
#[derive(Debug, Clone)]
pub struct Pagination {
    pub limit: i64,
    pub offset: i64,
}

impl Default for Pagination {
    fn default() -> Self {
        Pagination {
            limit: 20,
            offset: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Slug generation
// ---------------------------------------------------------------------------

/// Reserved slug names that cannot be used for repositories.
const RESERVED_SLUGS: &[&str] = &[
    "settings", "admin", "new", "api", "auth", "login",
];

/// Generate a URL-friendly slug from a repository name.
///
/// Rules:
/// 1. Lowercase the name.
/// 2. Replace any non-alphanumeric character with a hyphen.
/// 3. Collapse consecutive hyphens into a single hyphen.
/// 4. Trim leading and trailing hyphens.
pub fn generate_slug(name: &str) -> String {
    let lowered = name.to_lowercase();

    // Replace non-alphanumeric with hyphens.
    let replaced: String = lowered
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens.
    let mut slug = String::with_capacity(replaced.len());
    let mut prev_hyphen = false;
    for c in replaced.chars() {
        if c == '-' {
            if !prev_hyphen {
                slug.push('-');
            }
            prev_hyphen = true;
        } else {
            slug.push(c);
            prev_hyphen = false;
        }
    }

    // Trim leading and trailing hyphens.
    let slug = slug.trim_matches('-').to_string();

    slug
}

/// Validate that a slug is acceptable: not empty, not too long, and not
/// reserved.
pub fn validate_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() {
        return Err("repository name produces an empty slug".to_string());
    }
    if slug.len() > 128 {
        return Err("repository slug must be at most 128 characters".to_string());
    }
    if RESERVED_SLUGS.contains(&slug) {
        return Err(format!("'{}' is a reserved name and cannot be used", slug));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Visibility tests ---------------------------------------------------

    #[test]
    fn visibility_as_str_round_trip() {
        for v in &[Visibility::Public, Visibility::Private] {
            let s = v.as_str();
            let parsed = Visibility::from_db_str(s).unwrap();
            assert_eq!(*v, parsed);
        }
    }

    #[test]
    fn visibility_from_db_str_unknown() {
        assert!(Visibility::from_db_str("internal").is_none());
    }

    #[test]
    fn visibility_default_is_private() {
        assert_eq!(Visibility::default(), Visibility::Private);
    }

    #[test]
    fn visibility_display() {
        assert_eq!(Visibility::Public.to_string(), "public");
        assert_eq!(Visibility::Private.to_string(), "private");
    }

    #[test]
    fn visibility_serde_serialize() {
        let json = serde_json::to_string(&Visibility::Public).unwrap();
        assert_eq!(json, r#""public""#);
        let json = serde_json::to_string(&Visibility::Private).unwrap();
        assert_eq!(json, r#""private""#);
    }

    #[test]
    fn visibility_serde_deserialize() {
        let v: Visibility = serde_json::from_str(r#""public""#).unwrap();
        assert_eq!(v, Visibility::Public);
        let v: Visibility = serde_json::from_str(r#""private""#).unwrap();
        assert_eq!(v, Visibility::Private);
    }

    // -- Slug generation tests ----------------------------------------------

    #[test]
    fn slug_basic() {
        assert_eq!(generate_slug("My Repo"), "my-repo");
    }

    #[test]
    fn slug_special_chars() {
        assert_eq!(generate_slug("hello_world!@#test"), "hello-world-test");
    }

    #[test]
    fn slug_consecutive_special() {
        assert_eq!(generate_slug("a---b___c"), "a-b-c");
    }

    #[test]
    fn slug_leading_trailing() {
        assert_eq!(generate_slug("--hello--"), "hello");
    }

    #[test]
    fn slug_uppercase() {
        assert_eq!(generate_slug("MyAwesomeProject"), "myawesomeproject");
    }

    #[test]
    fn slug_mixed() {
        assert_eq!(generate_slug("  My Cool Repo! "), "my-cool-repo");
    }

    #[test]
    fn slug_numbers() {
        assert_eq!(generate_slug("project-123"), "project-123");
    }

    #[test]
    fn slug_all_special_chars_produces_empty() {
        assert_eq!(generate_slug("!!!"), "");
    }

    // -- Slug validation tests ----------------------------------------------

    #[test]
    fn validate_slug_ok() {
        assert!(validate_slug("my-repo").is_ok());
    }

    #[test]
    fn validate_slug_empty() {
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn validate_slug_reserved_settings() {
        assert!(validate_slug("settings").is_err());
    }

    #[test]
    fn validate_slug_reserved_admin() {
        assert!(validate_slug("admin").is_err());
    }

    #[test]
    fn validate_slug_reserved_new() {
        assert!(validate_slug("new").is_err());
    }

    #[test]
    fn validate_slug_reserved_api() {
        assert!(validate_slug("api").is_err());
    }

    #[test]
    fn validate_slug_reserved_auth() {
        assert!(validate_slug("auth").is_err());
    }

    #[test]
    fn validate_slug_reserved_login() {
        assert!(validate_slug("login").is_err());
    }

    #[test]
    fn validate_slug_too_long() {
        let long = "a".repeat(129);
        assert!(validate_slug(&long).is_err());
    }

    #[test]
    fn validate_slug_max_length_ok() {
        let max = "a".repeat(128);
        assert!(validate_slug(&max).is_ok());
    }

    // -- RepoResponse from Repo ---------------------------------------------

    #[test]
    fn repo_response_from_repo() {
        let now = Utc::now();
        let repo = Repo {
            id: Uuid::nil(),
            owner_id: Uuid::nil(),
            name: "test".to_string(),
            slug: "test".to_string(),
            description: Some("A test repo".to_string()),
            visibility: Visibility::Public,
            default_branch: "main".to_string(),
            archived: false,
            deleted_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        let response = RepoResponse::from(repo);
        assert_eq!(response.name, "test");
        assert_eq!(response.visibility, Visibility::Public);
        // RepoResponse should not have deleted_at field
    }

    // -- CreateRepoInput deserialization ------------------------------------

    #[test]
    fn create_repo_input_minimal() {
        let json = r#"{"name": "my-repo"}"#;
        let input: CreateRepoInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.name, "my-repo");
        assert!(input.description.is_none());
        assert!(input.visibility.is_none());
    }

    #[test]
    fn create_repo_input_full() {
        let json = r#"{"name": "my-repo", "description": "A repo", "visibility": "public"}"#;
        let input: CreateRepoInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.name, "my-repo");
        assert_eq!(input.description.as_deref(), Some("A repo"));
        assert_eq!(input.visibility, Some(Visibility::Public));
    }

    // -- UpdateRepoInput deserialization ------------------------------------

    #[test]
    fn update_repo_input_partial() {
        let json = r#"{"name": "new-name"}"#;
        let input: UpdateRepoInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.name.as_deref(), Some("new-name"));
        assert!(input.description.is_none());
    }

    #[test]
    fn update_repo_input_empty() {
        let json = r#"{}"#;
        let input: UpdateRepoInput = serde_json::from_str(json).unwrap();
        assert!(input.name.is_none());
        assert!(input.description.is_none());
    }
}

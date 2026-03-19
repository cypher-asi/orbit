use std::path::Path;

use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;
use crate::permissions::models::Role;
use crate::storage;

use super::models::{
    generate_slug, validate_slug, CreateRepoInput, Pagination, Repo, UpdateRepoInput, Visibility,
};

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

/// Create a new repository.
///
/// 1. Validates the name and generates a slug.
/// 2. Checks uniqueness of `(owner_id, slug)`.
/// 3. Inserts the repo row.
/// 4. Adds the owner as a `repo_members` entry with role `owner`.
/// 5. Initialises a bare Git repository on disk.
/// 6. Emits a `repo.created` audit event.
pub async fn create_repo(
    pool: &PgPool,
    storage_root: &Path,
    owner_id: Uuid,
    input: CreateRepoInput,
) -> Result<Repo, ApiError> {
    // 1. Generate & validate slug.
    let slug = generate_slug(&input.name);
    validate_slug(&slug).map_err(|msg| ApiError::BadRequest(msg))?;

    let visibility = input.visibility.unwrap_or(Visibility::Private);

    // 2-3. Insert the repo row (unique constraint enforces per-owner uniqueness).
    let repo = sqlx::query_as::<_, Repo>(
        r#"
        INSERT INTO repos (owner_id, name, slug, description, visibility)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING *
        "#,
    )
    .bind(owner_id)
    .bind(&input.name)
    .bind(&slug)
    .bind(&input.description)
    .bind(visibility)
    .fetch_one(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db_err) => {
            if db_err.code().as_deref() == Some("23505") {
                ApiError::Conflict(format!(
                    "a repository with slug '{}' already exists for this owner",
                    slug
                ))
            } else {
                ApiError::from(e)
            }
        }
        _ => ApiError::from(e),
    })?;

    // 4. Add the owner as a repo_member.
    sqlx::query(
        r#"
        INSERT INTO repo_members (repo_id, user_id, role)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(repo.id)
    .bind(owner_id)
    .bind(Role::Owner)
    .execute(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "create_repo: failed to insert repo_member");
        ApiError::Internal("internal server error".to_string())
    })?;

    // 5. Initialize bare Git repository on disk.
    storage::init_bare_repo(storage_root, repo.id).await?;

    // 6. Emit audit event.
    storage::emit_audit_event(
        pool,
        owner_id,
        "repo.created",
        Some(repo.id),
        None,
        Some(serde_json::json!({
            "name": repo.name,
            "slug": repo.slug,
            "visibility": repo.visibility.as_str(),
        })),
    )
    .await;

    Ok(repo)
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// Get a repository by its primary key ID.
///
/// Returns `None` if the repo does not exist or is soft-deleted.
pub async fn get_repo(pool: &PgPool, id: Uuid) -> Result<Option<Repo>, ApiError> {
    let repo = sqlx::query_as::<_, Repo>(
        r#"
        SELECT * FROM repos
        WHERE id = $1 AND deleted_at IS NULL
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(repo)
}

/// Get a repository by its owner ID and slug.
///
/// Returns `None` if not found or soft-deleted.
pub async fn get_repo_by_owner_and_slug(
    pool: &PgPool,
    owner_id: Uuid,
    slug: &str,
) -> Result<Option<Repo>, ApiError> {
    let repo = sqlx::query_as::<_, Repo>(
        r#"
        SELECT * FROM repos
        WHERE owner_id = $1 AND slug = $2 AND deleted_at IS NULL
        "#,
    )
    .bind(owner_id)
    .bind(slug)
    .fetch_optional(pool)
    .await?;

    Ok(repo)
}

/// List repositories owned by a specific user.
///
/// If `viewer` is `Some(user_id)` and matches `user_id`, all (non-deleted)
/// repos are returned. Otherwise only public repos are returned.
pub async fn list_repos_for_user(
    pool: &PgPool,
    user_id: Uuid,
    viewer: Option<Uuid>,
    pagination: Pagination,
) -> Result<Vec<Repo>, ApiError> {
    let is_self = viewer == Some(user_id);

    let repos = if is_self {
        // Show all repos for the owner.
        sqlx::query_as::<_, Repo>(
            r#"
            SELECT * FROM repos
            WHERE owner_id = $1 AND deleted_at IS NULL
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(user_id)
        .bind(pagination.limit)
        .bind(pagination.offset)
        .fetch_all(pool)
        .await?
    } else {
        // Show only public repos.
        sqlx::query_as::<_, Repo>(
            r#"
            SELECT * FROM repos
            WHERE owner_id = $1 AND deleted_at IS NULL AND visibility = 'public'
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(user_id)
        .bind(pagination.limit)
        .bind(pagination.offset)
        .fetch_all(pool)
        .await?
    };

    Ok(repos)
}

/// List all repositories accessible to a user.
///
/// Returns repos that are either:
/// - Owned by the user, OR
/// - The user is a member of (via `repo_members`), OR
/// - Public repos
///
/// Results are de-duplicated and ordered by creation time.
pub async fn list_accessible_repos(
    pool: &PgPool,
    user_id: Uuid,
    pagination: Pagination,
) -> Result<Vec<Repo>, ApiError> {
    let repos = sqlx::query_as::<_, Repo>(
        r#"
        SELECT DISTINCT r.* FROM repos r
        LEFT JOIN repo_members rm ON rm.repo_id = r.id AND rm.user_id = $1
        WHERE r.deleted_at IS NULL
          AND (r.owner_id = $1 OR rm.user_id IS NOT NULL OR r.visibility = 'public')
        ORDER BY r.created_at DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(user_id)
    .bind(pagination.limit)
    .bind(pagination.offset)
    .fetch_all(pool)
    .await?;

    Ok(repos)
}

/// List all repositories (admin-level) with pagination and optional name search.
///
/// Includes all non-deleted repos regardless of visibility or ownership.
pub async fn list_all_repos(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> Result<Vec<Repo>, ApiError> {
    match search {
        Some(q) if !q.is_empty() => {
            let pattern = format!("{}%", q);
            let repos = sqlx::query_as::<_, Repo>(
                r#"
                SELECT * FROM repos
                WHERE deleted_at IS NULL AND name ILIKE $3
                ORDER BY created_at DESC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit)
            .bind(offset)
            .bind(&pattern)
            .fetch_all(pool)
            .await?;
            Ok(repos)
        }
        _ => {
            let repos = sqlx::query_as::<_, Repo>(
                r#"
                SELECT * FROM repos
                WHERE deleted_at IS NULL
                ORDER BY created_at DESC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await?;
            Ok(repos)
        }
    }
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

/// Update a repository's metadata (name, description).
///
/// If the name changes, the slug is re-derived and uniqueness is
/// re-validated.
pub async fn update_repo(
    pool: &PgPool,
    id: Uuid,
    input: UpdateRepoInput,
) -> Result<Repo, ApiError> {
    // If name is being changed, re-derive and validate slug.
    let new_slug = match &input.name {
        Some(name) => {
            let slug = generate_slug(name);
            validate_slug(&slug).map_err(|msg| ApiError::BadRequest(msg))?;
            Some(slug)
        }
        None => None,
    };

    let repo = sqlx::query_as::<_, Repo>(
        r#"
        UPDATE repos
        SET name = COALESCE($2, name),
            slug = COALESCE($3, slug),
            description = COALESCE($4, description),
            updated_at = now()
        WHERE id = $1 AND deleted_at IS NULL
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&input.name)
    .bind(&new_slug)
    .bind(&input.description)
    .fetch_optional(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db_err) => {
            if db_err.code().as_deref() == Some("23505") {
                ApiError::Conflict(
                    "a repository with this slug already exists for this owner".to_string(),
                )
            } else {
                ApiError::from(e)
            }
        }
        _ => ApiError::from(e),
    })?
    .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    Ok(repo)
}

// ---------------------------------------------------------------------------
// Archive
// ---------------------------------------------------------------------------

/// Archive a repository, preventing further writes.
///
/// Emits a `repo.archived` audit event.
pub async fn archive_repo(pool: &PgPool, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE repos
        SET archived = true, updated_at = now()
        WHERE id = $1 AND deleted_at IS NULL
        "#,
    )
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("repository not found".to_string()));
    }

    storage::emit_audit_event(pool, actor_id, "repo.archived", Some(id), None, None).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Soft-delete
// ---------------------------------------------------------------------------

/// Soft-delete a repository by setting `deleted_at`.
///
/// The repository will be excluded from all subsequent queries.
/// Emits a `repo.deleted` audit event.
pub async fn delete_repo(pool: &PgPool, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE repos
        SET deleted_at = now(), updated_at = now()
        WHERE id = $1 AND deleted_at IS NULL
        "#,
    )
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("repository not found".to_string()));
    }

    storage::emit_audit_event(pool, actor_id, "repo.deleted", Some(id), None, None).await;

    Ok(())
}

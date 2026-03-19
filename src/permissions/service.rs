use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;

use super::models::{Permission, RepoAccessRow, RepoMember, Role};

/// Check whether a user (or unauthenticated visitor) has the required
/// permission level on a repository.
pub async fn check_repo_access(
    pool: &PgPool,
    user_id: Option<Uuid>,
    repo_id: Uuid,
    required: Permission,
) -> Result<(), ApiError> {
    let repo = sqlx::query_as::<_, RepoAccessRow>(
        r#"
        SELECT visibility, archived
        FROM repos
        WHERE id = $1 AND deleted_at IS NULL
        "#,
    )
    .bind(repo_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "check_repo_access: failed to load repo");
        ApiError::Internal("internal server error".to_string())
    })?
    .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    let is_public = repo.visibility == "public";

    // Public repo + read access = allow (even for unauthenticated users).
    if is_public && required == Permission::Read {
        return Ok(());
    }

    // Unauthenticated user on a non-public-read path.
    let uid = match user_id {
        Some(uid) => uid,
        None => {
            if is_public {
                return Err(ApiError::Unauthorized(
                    "authentication required".to_string(),
                ));
            }
            return Err(ApiError::NotFound("repository not found".to_string()));
        }
    };

    // Load the user's role on this repo.
    let role = get_user_role(pool, uid, repo_id).await?;

    match role {
        None => {
            if is_public {
                if required == Permission::Read {
                    return Ok(());
                }
                return Err(ApiError::Forbidden("insufficient permissions".to_string()));
            }
            return Err(ApiError::NotFound("repository not found".to_string()));
        }
        Some(r) => {
            if repo.archived && matches!(required, Permission::Write | Permission::Admin) {
                return Err(ApiError::Forbidden("repository is archived".to_string()));
            }

            if !r.has_permission(required) {
                return Err(ApiError::Forbidden("insufficient permissions".to_string()));
            }
        }
    }

    Ok(())
}

/// Get the role of a user on a specific repository.
pub async fn get_user_role(
    pool: &PgPool,
    user_id: Uuid,
    repo_id: Uuid,
) -> Result<Option<Role>, ApiError> {
    let row = sqlx::query_scalar::<_, Role>(
        r#"
        SELECT role FROM repo_members
        WHERE repo_id = $1 AND user_id = $2
        "#,
    )
    .bind(repo_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "get_user_role: failed to query repo_members");
        ApiError::Internal("internal server error".to_string())
    })?;

    Ok(row)
}

/// Add a collaborator to a repository with the given role.
pub async fn add_collaborator(
    pool: &PgPool,
    repo_id: Uuid,
    user_id: Uuid,
    role: Role,
) -> Result<RepoMember, ApiError> {
    let member = sqlx::query_as::<_, RepoMember>(
        r#"
        INSERT INTO repo_members (repo_id, user_id, role)
        VALUES ($1, $2, $3)
        ON CONFLICT (repo_id, user_id)
        DO UPDATE SET role = EXCLUDED.role
        RETURNING *
        "#,
    )
    .bind(repo_id)
    .bind(user_id)
    .bind(role)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "add_collaborator: database error");
        ApiError::Internal("internal server error".to_string())
    })?;

    Ok(member)
}

/// Remove a collaborator from a repository.
pub async fn remove_collaborator(
    pool: &PgPool,
    repo_id: Uuid,
    user_id: Uuid,
) -> Result<(), ApiError> {
    let existing_role = get_user_role(pool, user_id, repo_id).await?;
    if existing_role == Some(Role::Owner) {
        return Err(ApiError::Forbidden(
            "cannot remove the repository owner".to_string(),
        ));
    }

    let result = sqlx::query(
        r#"
        DELETE FROM repo_members
        WHERE repo_id = $1 AND user_id = $2
        "#,
    )
    .bind(repo_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "remove_collaborator: database error");
        ApiError::Internal("internal server error".to_string())
    })?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("collaborator not found".to_string()));
    }

    Ok(())
}

/// Update a collaborator's role on a repository.
pub async fn update_collaborator_role(
    pool: &PgPool,
    repo_id: Uuid,
    user_id: Uuid,
    role: Role,
) -> Result<RepoMember, ApiError> {
    let existing_role = get_user_role(pool, user_id, repo_id).await?;
    if existing_role == Some(Role::Owner) {
        return Err(ApiError::Forbidden(
            "cannot change the repository owner's role".to_string(),
        ));
    }

    let member = sqlx::query_as::<_, RepoMember>(
        r#"
        UPDATE repo_members
        SET role = $3
        WHERE repo_id = $1 AND user_id = $2
        RETURNING *
        "#,
    )
    .bind(repo_id)
    .bind(user_id)
    .bind(role)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "update_collaborator_role: database error");
        ApiError::Internal("internal server error".to_string())
    })?
    .ok_or_else(|| ApiError::NotFound("collaborator not found".to_string()))?;

    Ok(member)
}

/// List all collaborators of a repository.
pub async fn list_collaborators(pool: &PgPool, repo_id: Uuid) -> Result<Vec<RepoMember>, ApiError> {
    let members = sqlx::query_as::<_, RepoMember>(
        r#"
        SELECT * FROM repo_members
        WHERE repo_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "list_collaborators: database error");
        ApiError::Internal("internal server error".to_string())
    })?;

    Ok(members)
}

#[cfg(test)]
mod tests {
    use crate::permissions::models::{Permission, Role};

    #[test]
    fn owner_role_satisfies_all_permissions() {
        assert!(Role::Owner.has_permission(Permission::Read));
        assert!(Role::Owner.has_permission(Permission::Write));
        assert!(Role::Owner.has_permission(Permission::Admin));
    }

    #[test]
    fn writer_role_satisfies_read_and_write() {
        assert!(Role::Writer.has_permission(Permission::Read));
        assert!(Role::Writer.has_permission(Permission::Write));
        assert!(!Role::Writer.has_permission(Permission::Admin));
    }

    #[test]
    fn reader_role_satisfies_read_only() {
        assert!(Role::Reader.has_permission(Permission::Read));
        assert!(!Role::Reader.has_permission(Permission::Write));
        assert!(!Role::Reader.has_permission(Permission::Admin));
    }
}

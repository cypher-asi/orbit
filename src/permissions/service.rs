use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;

use super::models::{Permission, RepoMember, RepoRow, Role};

/// Check whether a user (or unauthenticated visitor) has the required
/// permission level on a repository.
///
/// # Behaviour
/// 1. Load the repo; return `NotFound` if it does not exist (or is soft-deleted).
/// 2. If the user is authenticated and `is_admin`, grant access immediately.
/// 3. For **public** repos with `Permission::Read`, allow unauthenticated access.
/// 4. For **private** repos accessed by an unauthenticated user, return `NotFound`
///    (not `Forbidden`) to avoid leaking the repo's existence.
/// 5. Load the caller's membership role. If they have no membership on a
///    private repo, return `NotFound`. If they have insufficient permission,
///    return `Forbidden`.
/// 6. Archived repos reject all `Write` and `Admin` operations regardless of
///    role.
pub async fn check_repo_access(
    pool: &PgPool,
    user_id: Option<Uuid>,
    repo_id: Uuid,
    required: Permission,
) -> Result<(), ApiError> {
    // 1. Load the repo.
    let repo = sqlx::query_as::<_, RepoRow>(
        r#"
        SELECT id, owner_id, visibility, archived
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

    // 2. If the caller is authenticated, check admin status.
    if let Some(uid) = user_id {
        let is_admin = is_user_admin(pool, uid).await?;
        if is_admin {
            // Admins bypass all checks, but archived repos still block writes.
            if repo.archived && matches!(required, Permission::Write | Permission::Admin) {
                return Err(ApiError::Forbidden(
                    "repository is archived".to_string(),
                ));
            }
            return Ok(());
        }
    }

    // 3. Public repo + read access = allow (even for unauthenticated users).
    if is_public && required == Permission::Read {
        return Ok(());
    }

    // 4. Unauthenticated user on a non-public-read path.
    let uid = match user_id {
        Some(uid) => uid,
        None => {
            if is_public {
                // Public repo but requires write/admin -- need authentication.
                return Err(ApiError::Forbidden(
                    "authentication required".to_string(),
                ));
            }
            // Private repo -- hide existence.
            return Err(ApiError::NotFound("repository not found".to_string()));
        }
    };

    // 5. Load the user's role on this repo.
    let role = get_user_role(pool, uid, repo_id).await?;

    match role {
        None => {
            if is_public {
                // Public repo, authenticated user with no membership.
                // They can read but not write/admin.
                if required == Permission::Read {
                    return Ok(());
                }
                return Err(ApiError::Forbidden(
                    "insufficient permissions".to_string(),
                ));
            }
            // Private repo, no membership -- hide existence.
            return Err(ApiError::NotFound("repository not found".to_string()));
        }
        Some(r) => {
            // 6. Check archived status before role check.
            if repo.archived && matches!(required, Permission::Write | Permission::Admin) {
                return Err(ApiError::Forbidden(
                    "repository is archived".to_string(),
                ));
            }

            if !r.has_permission(required) {
                return Err(ApiError::Forbidden(
                    "insufficient permissions".to_string(),
                ));
            }
        }
    }

    Ok(())
}

/// Get the role of a user on a specific repository.
///
/// Returns `None` if the user is not a collaborator on the repo.
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
///
/// If the user is already a collaborator, their role is updated (upsert).
/// Returns the resulting `RepoMember` with joined user info.
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
        RETURNING id, repo_id, user_id, role, created_at,
                  (SELECT username FROM users WHERE id = $2) AS username,
                  (SELECT display_name FROM users WHERE id = $2) AS display_name
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
///
/// Returns `Forbidden` if the target user is the repo owner (owners cannot
/// be removed). Returns `NotFound` if the user is not a collaborator.
pub async fn remove_collaborator(
    pool: &PgPool,
    repo_id: Uuid,
    user_id: Uuid,
) -> Result<(), ApiError> {
    // Prevent removing the owner.
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
///
/// Returns `Forbidden` if the target user is the repo owner (owner role
/// cannot be changed through this function).
/// Returns `NotFound` if the user is not a collaborator.
pub async fn update_collaborator_role(
    pool: &PgPool,
    repo_id: Uuid,
    user_id: Uuid,
    role: Role,
) -> Result<RepoMember, ApiError> {
    // Prevent changing the owner's role.
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
        RETURNING id, repo_id, user_id, role, created_at,
                  (SELECT username FROM users WHERE id = repo_members.user_id) AS username,
                  (SELECT display_name FROM users WHERE id = repo_members.user_id) AS display_name
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

/// List all collaborators of a repository with their user info.
pub async fn list_collaborators(
    pool: &PgPool,
    repo_id: Uuid,
) -> Result<Vec<RepoMember>, ApiError> {
    let members = sqlx::query_as::<_, RepoMember>(
        r#"
        SELECT rm.id, rm.repo_id, rm.user_id, rm.role, rm.created_at,
               u.username, u.display_name
        FROM repo_members rm
        JOIN users u ON u.id = rm.user_id
        WHERE rm.repo_id = $1
        ORDER BY rm.created_at ASC
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

/// Helper: check if a user has the `is_admin` flag set.
async fn is_user_admin(pool: &PgPool, user_id: Uuid) -> Result<bool, ApiError> {
    let is_admin = sqlx::query_scalar::<_, bool>(
        "SELECT is_admin FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "is_user_admin: failed to query user");
        ApiError::Internal("internal server error".to_string())
    })?
    .unwrap_or(false);

    Ok(is_admin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::models::{Permission, Role};

    // Unit tests for role-permission logic (no DB needed).

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

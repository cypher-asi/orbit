use sqlx::PgPool;
use uuid::Uuid;

use super::models::{CreateUserInput, UpdateUserInput, User};
use crate::errors::ApiError;

/// Create a new user and return the inserted row.
///
/// Returns `ApiError::Conflict` if the username or email already exists
/// (unique constraint violation).
pub async fn create_user(pool: &PgPool, input: CreateUserInput) -> Result<User, ApiError> {
    let user = sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (username, email, password_hash, display_name)
        VALUES ($1, $2, $3, $4)
        RETURNING *
        "#,
    )
    .bind(&input.username)
    .bind(&input.email)
    .bind(&input.password_hash)
    .bind(&input.display_name)
    .fetch_one(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db_err) => {
            // PostgreSQL unique-violation SQLSTATE: 23505
            if db_err.code().as_deref() == Some("23505") {
                let message = db_err.message().to_string();
                if message.contains("username") {
                    ApiError::Conflict("username already exists".to_string())
                } else if message.contains("email") {
                    ApiError::Conflict("email already exists".to_string())
                } else {
                    ApiError::Conflict("resource already exists".to_string())
                }
            } else {
                ApiError::from(e)
            }
        }
        _ => ApiError::from(e),
    })?;

    Ok(user)
}

/// Look up a user by their primary-key ID.
pub async fn get_user_by_id(pool: &PgPool, id: Uuid) -> Result<Option<User>, ApiError> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(user)
}

/// Look up a user by their unique username.
pub async fn get_user_by_username(pool: &PgPool, username: &str) -> Result<Option<User>, ApiError> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = $1")
        .bind(username)
        .fetch_optional(pool)
        .await?;

    Ok(user)
}

/// Update a user's profile fields. Only the fields that are `Some` in
/// `input` will be changed; `None` fields are left untouched.
///
/// Always bumps `updated_at` to `now()`.
///
/// Returns the updated user row, or `ApiError::NotFound` if the id does not
/// exist.
pub async fn update_user(
    pool: &PgPool,
    id: Uuid,
    input: UpdateUserInput,
) -> Result<User, ApiError> {
    let user = sqlx::query_as::<_, User>(
        r#"
        UPDATE users
        SET display_name = COALESCE($2, display_name),
            email = COALESCE($3, email),
            updated_at = now()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&input.display_name)
    .bind(&input.email)
    .fetch_optional(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db_err) => {
            if db_err.code().as_deref() == Some("23505") {
                ApiError::Conflict("email already exists".to_string())
            } else {
                ApiError::from(e)
            }
        }
        _ => ApiError::from(e),
    })?
    .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;

    Ok(user)
}

/// List users with pagination (limit / offset), ordered by creation time
/// (oldest first).
pub async fn list_users(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<User>, ApiError> {
    let users =
        sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at ASC LIMIT $1 OFFSET $2")
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await?;

    Ok(users)
}

/// List users with pagination and optional username search (ILIKE prefix).
pub async fn list_users_search(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> Result<Vec<User>, ApiError> {
    match search {
        Some(q) if !q.is_empty() => {
            let pattern = format!("{}%", q);
            let users = sqlx::query_as::<_, User>(
                r#"
                SELECT * FROM users
                WHERE username ILIKE $3
                ORDER BY created_at ASC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit)
            .bind(offset)
            .bind(&pattern)
            .fetch_all(pool)
            .await?;
            Ok(users)
        }
        _ => list_users(pool, limit, offset).await,
    }
}

/// Disable a user account by setting `is_disabled = true`.
///
/// Returns `ApiError::NotFound` if the user does not exist.
pub async fn disable_user(pool: &PgPool, id: Uuid) -> Result<(), ApiError> {
    let result =
        sqlx::query("UPDATE users SET is_disabled = true, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("user not found".to_string()));
    }

    Ok(())
}

/// Enable a user account by setting `is_disabled = false`.
///
/// Returns `ApiError::NotFound` if the user does not exist.
pub async fn enable_user(pool: &PgPool, id: Uuid) -> Result<(), ApiError> {
    let result =
        sqlx::query("UPDATE users SET is_disabled = false, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("user not found".to_string()));
    }

    Ok(())
}

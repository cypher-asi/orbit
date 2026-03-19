use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;
use super::models::{AuditEvent, EventFilter, NewAuditEvent};

/// Emit an audit event by inserting it into the `audit_events` table.
///
/// This is a fire-and-forget operation: if the insert fails, the error is
/// logged but **not** propagated to the caller. This ensures that audit
/// failures never break the parent operation.
pub async fn emit(pool: &PgPool, event: NewAuditEvent) {
    let result = sqlx::query(
        r#"
        INSERT INTO audit_events (actor_id, event_type, repo_id, target_id, metadata)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(event.actor_id)
    .bind(&event.event_type)
    .bind(event.repo_id)
    .bind(event.target_id)
    .bind(&event.metadata)
    .execute(pool)
    .await;

    match result {
        Ok(_) => {
            tracing::debug!(
                event_type = %event.event_type,
                actor_id = ?event.actor_id,
                repo_id = ?event.repo_id,
                "audit event emitted"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                event_type = %event.event_type,
                actor_id = ?event.actor_id,
                "failed to emit audit event"
            );
        }
    }
}

/// List audit events matching the given filter criteria with pagination.
///
/// Builds a dynamic query with optional WHERE clauses based on which
/// filter fields are set. Results are ordered by `created_at DESC`.
pub async fn list_events(
    pool: &PgPool,
    filter: EventFilter,
) -> Result<Vec<AuditEvent>, ApiError> {
    // Build the query dynamically using a QueryBuilder to handle optional filters.
    let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT id, actor_id, event_type, repo_id, target_id, metadata, created_at FROM audit_events WHERE 1=1"
    );

    if let Some(actor_id) = filter.actor_id {
        query.push(" AND actor_id = ");
        query.push_bind(actor_id);
    }

    if let Some(repo_id) = filter.repo_id {
        query.push(" AND repo_id = ");
        query.push_bind(repo_id);
    }

    if let Some(ref event_type) = filter.event_type {
        query.push(" AND event_type = ");
        query.push_bind(event_type.clone());
    }

    if let Some(since) = filter.since {
        query.push(" AND created_at >= ");
        query.push_bind(since);
    }

    if let Some(until) = filter.until {
        query.push(" AND created_at <= ");
        query.push_bind(until);
    }

    query.push(" ORDER BY created_at DESC");

    query.push(" LIMIT ");
    query.push_bind(filter.limit as i64);

    query.push(" OFFSET ");
    query.push_bind(filter.offset as i64);

    let events = query
        .build_query_as::<AuditEvent>()
        .fetch_all(pool)
        .await?;

    Ok(events)
}

/// Retrieve a single audit event by its ID.
///
/// Returns `None` if no event with the given ID exists.
pub async fn get_event(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<AuditEvent>, ApiError> {
    let event = sqlx::query_as::<_, AuditEvent>(
        r#"
        SELECT id, actor_id, event_type, repo_id, target_id, metadata, created_at
        FROM audit_events
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(event)
}

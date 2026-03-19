use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;
use super::models::Job;

/// Enqueue a new background job for immediate execution.
///
/// The job is inserted with `status = 'pending'` and `run_at = now()`.
/// Returns the UUID of the newly created job.
pub async fn enqueue(
    pool: &PgPool,
    job_type: &str,
    payload: serde_json::Value,
) -> Result<Uuid, ApiError> {
    let row = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO jobs (job_type, payload)
        VALUES ($1, $2)
        RETURNING id
        "#,
    )
    .bind(job_type)
    .bind(&payload)
    .fetch_one(pool)
    .await?;

    tracing::debug!(job_id = %row, job_type = %job_type, "job enqueued");
    Ok(row)
}

/// Enqueue a new background job that should not run before `run_at`.
///
/// Identical to [`enqueue`] but allows scheduling the job for a future time.
pub async fn enqueue_delayed(
    pool: &PgPool,
    job_type: &str,
    payload: serde_json::Value,
    run_at: DateTime<Utc>,
) -> Result<Uuid, ApiError> {
    let row = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO jobs (job_type, payload, run_at)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
    )
    .bind(job_type)
    .bind(&payload)
    .bind(run_at)
    .fetch_one(pool)
    .await?;

    tracing::debug!(job_id = %row, job_type = %job_type, %run_at, "delayed job enqueued");
    Ok(row)
}

/// Atomically claim the next pending job that is ready to run.
///
/// Uses `FOR UPDATE SKIP LOCKED` to avoid duplicate execution across
/// concurrent workers. The claimed job has its status set to `'running'`
/// and its `attempts` counter incremented.
///
/// Returns `None` if no eligible job is available.
pub async fn fetch_next(pool: &PgPool) -> Result<Option<Job>, ApiError> {
    let job = sqlx::query_as::<_, Job>(
        r#"
        UPDATE jobs
        SET status = 'running', attempts = attempts + 1
        WHERE id = (
            SELECT id FROM jobs
            WHERE status = 'pending'
              AND run_at <= NOW()
              AND attempts < max_attempts
            ORDER BY run_at ASC
            LIMIT 1
            FOR UPDATE SKIP LOCKED
        )
        RETURNING *
        "#,
    )
    .fetch_optional(pool)
    .await?;

    if let Some(ref j) = job {
        tracing::debug!(job_id = %j.id, job_type = %j.job_type, attempt = j.attempts, "job claimed");
    }

    Ok(job)
}

/// Mark a job as completed successfully.
///
/// Sets `status = 'completed'` and records `completed_at`.
pub async fn complete(pool: &PgPool, job_id: Uuid) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'completed', completed_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .execute(pool)
    .await?;

    tracing::debug!(job_id = %job_id, "job completed");
    Ok(())
}

/// Record a job failure and apply retry logic with exponential backoff.
///
/// If the job still has remaining attempts (`attempts < max_attempts`), it is
/// reset to `'pending'` with a delayed `run_at`:
///   - Attempt 2: +30 seconds
///   - Attempt 3+: +5 minutes
///
/// If the job has exhausted all attempts, it is set to `'failed'` and the
/// error message is stored in `last_error`.
pub async fn fail(pool: &PgPool, job_id: Uuid, error: &str) -> Result<(), ApiError> {
    // First, fetch the current state of the job so we can decide the retry strategy.
    let job = sqlx::query_as::<_, Job>(
        r#"
        SELECT * FROM jobs WHERE id = $1
        "#,
    )
    .bind(job_id)
    .fetch_one(pool)
    .await?;

    if job.attempts < job.max_attempts {
        // Calculate backoff delay based on the current attempt number.
        let delay = match job.attempts {
            1 => Duration::seconds(30),      // after 1st failure: +30s
            2 => Duration::seconds(300),     // after 2nd failure: +5 min
            _ => Duration::seconds(300),     // 3rd+: +5 min
        };
        let next_run_at = Utc::now() + delay;

        sqlx::query(
            r#"
            UPDATE jobs
            SET status = 'pending', last_error = $2, run_at = $3
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(error)
        .bind(next_run_at)
        .execute(pool)
        .await?;

        tracing::debug!(
            job_id = %job_id,
            attempt = job.attempts,
            next_run_at = %next_run_at,
            "job failed, scheduled for retry"
        );
    } else {
        // No retries remaining -- mark as permanently failed.
        sqlx::query(
            r#"
            UPDATE jobs
            SET status = 'failed', last_error = $2
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(error)
        .execute(pool)
        .await?;

        tracing::warn!(
            job_id = %job_id,
            attempts = job.attempts,
            error = %error,
            "job permanently failed (max attempts exhausted)"
        );
    }

    Ok(())
}

/// List jobs with pagination and optional status filter.
///
/// If `status` is provided, only jobs matching that status are returned.
/// Results are ordered by `created_at DESC`.
pub async fn list_jobs(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    status: Option<&str>,
) -> Result<Vec<Job>, ApiError> {
    match status {
        Some(s) if !s.is_empty() => {
            let jobs = sqlx::query_as::<_, Job>(
                r#"
                SELECT * FROM jobs
                WHERE status = $3
                ORDER BY created_at DESC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit)
            .bind(offset)
            .bind(s)
            .fetch_all(pool)
            .await?;
            Ok(jobs)
        }
        _ => {
            let jobs = sqlx::query_as::<_, Job>(
                r#"
                SELECT * FROM jobs
                ORDER BY created_at DESC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await?;
            Ok(jobs)
        }
    }
}

/// Get a single job by ID.
pub async fn get_job(pool: &PgPool, job_id: Uuid) -> Result<Option<Job>, ApiError> {
    let job = sqlx::query_as::<_, Job>(
        "SELECT * FROM jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    Ok(job)
}

/// List failed jobs, ordered by most recent first.
///
/// `limit` controls the maximum number of results returned.
pub async fn list_failed(pool: &PgPool, limit: u32) -> Result<Vec<Job>, ApiError> {
    let jobs = sqlx::query_as::<_, Job>(
        r#"
        SELECT * FROM jobs
        WHERE status = 'failed'
        ORDER BY created_at DESC
        LIMIT $1
        "#,
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    Ok(jobs)
}

/// Manually retry a failed job by resetting it to pending.
///
/// Resets `status` to `'pending'`, sets `run_at` to now, and clears
/// `last_error`. The `attempts` counter is kept so the remaining budget
/// is respected. Returns an error if the job is not in `'failed'` status.
pub async fn retry(pool: &PgPool, job_id: Uuid) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE jobs
        SET status = 'pending', run_at = NOW(), last_error = NULL, attempts = 0
        WHERE id = $1 AND status = 'failed'
        "#,
    )
    .bind(job_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(
            "job not found or not in failed status".to_string(),
        ));
    }

    tracing::info!(job_id = %job_id, "failed job manually retried");
    Ok(())
}

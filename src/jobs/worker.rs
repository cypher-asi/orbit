use std::path::PathBuf;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

use crate::errors::ApiError;
use crate::storage::service::StorageConfig;

use super::models::Job;

/// Run the background job worker loop.
///
/// The worker repeatedly polls for pending jobs, dispatches them to the
/// appropriate handler based on `job_type`, and marks them as completed
/// or failed.  When no jobs are available, it sleeps for 5 seconds before
/// polling again.
///
/// The loop respects graceful shutdown: it listens on `shutdown_rx` and
/// will finish processing the current job (if any) before returning.
pub async fn run_worker(
    pool: PgPool,
    storage: StorageConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tracing::info!("background job worker started");

    loop {
        // Check if we should shut down before fetching the next job.
        if *shutdown_rx.borrow() {
            tracing::info!("background job worker received shutdown signal, exiting");
            break;
        }

        match super::fetch_next(&pool).await {
            Ok(Some(job)) => {
                let job_id = job.id;
                let job_type = job.job_type.clone();
                tracing::info!(
                    job_id = %job_id,
                    job_type = %job_type,
                    attempt = job.attempts,
                    "executing job"
                );

                let result = execute_job(&pool, &storage, &job).await;

                match result {
                    Ok(()) => {
                        if let Err(e) = super::complete(&pool, job_id).await {
                            tracing::error!(
                                job_id = %job_id,
                                error = %e,
                                "failed to mark job as completed"
                            );
                        } else {
                            tracing::info!(
                                job_id = %job_id,
                                job_type = %job_type,
                                "job completed successfully"
                            );
                        }
                    }
                    Err(e) => {
                        let error_msg = e.to_string();
                        tracing::warn!(
                            job_id = %job_id,
                            job_type = %job_type,
                            error = %error_msg,
                            "job execution failed"
                        );
                        if let Err(fail_err) = super::fail(&pool, job_id, &error_msg).await {
                            tracing::error!(
                                job_id = %job_id,
                                error = %fail_err,
                                "failed to record job failure"
                            );
                        }
                    }
                }
            }
            Ok(None) => {
                // No pending jobs -- sleep, but wake up early on shutdown.
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = shutdown_rx.changed() => {
                        tracing::info!("background job worker received shutdown signal during sleep, exiting");
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "error fetching next job");
                // Back off on error to avoid tight-looping.
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = shutdown_rx.changed() => {
                        tracing::info!("background job worker received shutdown signal during error backoff, exiting");
                        break;
                    }
                }
            }
        }
    }

    tracing::info!("background job worker shut down");
}

/// Dispatch a job to the appropriate handler based on its `job_type`.
async fn execute_job(_pool: &PgPool, storage: &StorageConfig, job: &Job) -> Result<(), ApiError> {
    match job.job_type.as_str() {
        "cleanup_worktree" => handle_cleanup_worktree(storage, job).await,
        "delete_repo_storage" => handle_delete_repo_storage(storage, job).await,
        "repo_maintenance" => handle_repo_maintenance(storage, job).await,
        other => {
            tracing::warn!(job_type = %other, job_id = %job.id, "unknown job type");
            Err(ApiError::Internal(format!("unknown job type: {}", other)))
        }
    }
}

/// Handle `cleanup_worktree` jobs.
///
/// Expects `payload.path` to contain the worktree directory path to remove.
/// If the path does not exist, the job succeeds silently (idempotent).
async fn handle_cleanup_worktree(storage: &StorageConfig, job: &Job) -> Result<(), ApiError> {
    let path_str = job
        .payload
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::Internal("cleanup_worktree job missing 'path' in payload".to_string())
        })?;

    let worktree_path = PathBuf::from(path_str);

    // Safety: only allow paths under the storage root to prevent directory
    // traversal attacks.
    if !worktree_path.starts_with(&storage.root_path) {
        return Err(ApiError::Internal(
            "worktree path is outside of storage root".to_string(),
        ));
    }

    if !worktree_path.exists() {
        tracing::info!(
            job_id = %job.id,
            path = %worktree_path.display(),
            "worktree directory does not exist, nothing to clean up"
        );
        return Ok(());
    }

    tokio::fs::remove_dir_all(&worktree_path)
        .await
        .map_err(|e| {
            tracing::error!(
                error = %e,
                path = %worktree_path.display(),
                "failed to remove worktree directory"
            );
            ApiError::Internal(format!("failed to remove worktree: {}", e))
        })?;

    tracing::info!(
        job_id = %job.id,
        path = %worktree_path.display(),
        "cleaned up worktree directory"
    );

    Ok(())
}

/// Handle `delete_repo_storage` jobs.
///
/// Expects `payload.repo_id` to contain the UUID of the repository whose
/// on-disk storage should be deleted.
async fn handle_delete_repo_storage(storage: &StorageConfig, job: &Job) -> Result<(), ApiError> {
    let repo_id_str = job
        .payload
        .get("repo_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::Internal("delete_repo_storage job missing 'repo_id' in payload".to_string())
        })?;

    let repo_id: Uuid = repo_id_str
        .parse()
        .map_err(|_| ApiError::Internal(format!("invalid repo_id in payload: {}", repo_id_str)))?;

    crate::storage::service::delete_repo(storage, repo_id).await?;

    tracing::info!(
        job_id = %job.id,
        repo_id = %repo_id,
        "deleted repo storage"
    );

    Ok(())
}

/// Handle `repo_maintenance` jobs.
///
/// Expects `payload.repo_id` to contain the UUID of the repository on
/// which to run `git gc`.
async fn handle_repo_maintenance(storage: &StorageConfig, job: &Job) -> Result<(), ApiError> {
    let repo_id_str = job
        .payload
        .get("repo_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::Internal("repo_maintenance job missing 'repo_id' in payload".to_string())
        })?;

    let repo_id: Uuid = repo_id_str
        .parse()
        .map_err(|_| ApiError::Internal(format!("invalid repo_id in payload: {}", repo_id_str)))?;

    let repo_path = crate::storage::service::repo_path(storage, repo_id);

    if !repo_path.exists() {
        tracing::warn!(
            job_id = %job.id,
            repo_id = %repo_id,
            path = %repo_path.display(),
            "repo directory does not exist, skipping maintenance"
        );
        return Ok(());
    }

    let output = tokio::process::Command::new("git")
        .arg("gc")
        .env("GIT_DIR", &repo_path)
        .output()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to execute git gc");
            ApiError::Internal(format!("failed to execute git gc: {}", e))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(
            job_id = %job.id,
            repo_id = %repo_id,
            stderr = %stderr,
            "git gc failed"
        );
        return Err(ApiError::Internal(format!("git gc failed: {}", stderr)));
    }

    tracing::info!(
        job_id = %job.id,
        repo_id = %repo_id,
        "repo maintenance (git gc) completed"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_job(job_type: &str, payload: serde_json::Value) -> Job {
        Job {
            id: Uuid::new_v4(),
            job_type: job_type.to_string(),
            payload,
            status: "running".to_string(),
            attempts: 1,
            max_attempts: 3,
            last_error: None,
            run_at: Utc::now(),
            completed_at: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn cleanup_worktree_removes_directory() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        // Create a fake worktree directory under the storage root.
        let worktree_dir = tmp.path().join("worktrees").join("test-wt");
        tokio::fs::create_dir_all(&worktree_dir)
            .await
            .expect("create worktree dir");

        let job = make_job(
            "cleanup_worktree",
            serde_json::json!({ "path": worktree_dir.to_str().unwrap() }),
        );

        let result = handle_cleanup_worktree(&storage, &job).await;
        assert!(result.is_ok());
        assert!(!worktree_dir.exists());
    }

    #[tokio::test]
    async fn cleanup_worktree_succeeds_if_not_exists() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let worktree_dir = tmp.path().join("nonexistent");
        let job = make_job(
            "cleanup_worktree",
            serde_json::json!({ "path": worktree_dir.to_str().unwrap() }),
        );

        let result = handle_cleanup_worktree(&storage, &job).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn cleanup_worktree_rejects_path_outside_storage() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let job = make_job(
            "cleanup_worktree",
            serde_json::json!({ "path": "/tmp/evil-path" }),
        );

        let result = handle_cleanup_worktree(&storage, &job).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cleanup_worktree_missing_path_payload() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let job = make_job("cleanup_worktree", serde_json::json!({}));
        let result = handle_cleanup_worktree(&storage, &job).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_repo_storage_deletes_repo() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let repo_id = Uuid::new_v4();
        // Create the repo directory structure that repo_path expects.
        let path = crate::storage::service::repo_path(&storage, repo_id);
        tokio::fs::create_dir_all(&path)
            .await
            .expect("create repo dir");
        assert!(path.exists());

        let job = make_job(
            "delete_repo_storage",
            serde_json::json!({ "repo_id": repo_id.to_string() }),
        );

        let result = handle_delete_repo_storage(&storage, &job).await;
        assert!(result.is_ok());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn delete_repo_storage_missing_repo_id() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let job = make_job("delete_repo_storage", serde_json::json!({}));
        let result = handle_delete_repo_storage(&storage, &job).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn repo_maintenance_succeeds_if_repo_missing() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let repo_id = Uuid::new_v4();
        let job = make_job(
            "repo_maintenance",
            serde_json::json!({ "repo_id": repo_id.to_string() }),
        );

        // Should succeed even if repo does not exist on disk.
        let result = handle_repo_maintenance(&storage, &job).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn repo_maintenance_missing_repo_id() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let job = make_job("repo_maintenance", serde_json::json!({}));
        let result = handle_repo_maintenance(&storage, &job).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn repo_maintenance_runs_git_gc() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let storage = StorageConfig::new(tmp.path().to_path_buf());

        let repo_id = Uuid::new_v4();

        // Create a real bare repo so git gc has something to work on.
        let repo_path = crate::storage::service::repo_path(&storage, repo_id);
        tokio::fs::create_dir_all(&repo_path)
            .await
            .expect("create repo dir");

        let output = tokio::process::Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&repo_path)
            .output()
            .await
            .expect("git init");
        assert!(output.status.success());

        let job = make_job(
            "repo_maintenance",
            serde_json::json!({ "repo_id": repo_id.to_string() }),
        );

        let result = handle_repo_maintenance(&storage, &job).await;
        assert!(result.is_ok());
    }
}

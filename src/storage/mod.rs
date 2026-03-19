pub mod git;
pub mod service;

use sqlx::PgPool;
use std::path::Path;
use uuid::Uuid;

use crate::errors::ApiError;

/// Initialize a bare Git repository on disk for the given repo ID.
///
/// Uses fan-out layout: `<storage_root>/<prefix>/<repo_id>.git`
/// where prefix is the first 2 characters of the UUID.
/// If the directory already exists this is a no-op (idempotent).
pub async fn init_bare_repo(storage_root: &Path, repo_id: Uuid) -> Result<(), ApiError> {
    let id_str = repo_id.to_string();
    let prefix = &id_str[..2];
    let repo_path = storage_root.join(prefix).join(format!("{}.git", id_str));

    if repo_path.exists() {
        tracing::warn!(
            path = %repo_path.display(),
            "bare repo directory already exists, skipping init"
        );
        return Ok(());
    }

    // Create the directory structure.
    std::fs::create_dir_all(&repo_path).map_err(|e| {
        tracing::error!(error = %e, path = %repo_path.display(), "failed to create bare repo directory");
        ApiError::Internal("failed to initialize repository storage".to_string())
    })?;

    // Run `git init --bare` in the directory.
    let output = std::process::Command::new("git")
        .arg("init")
        .arg("--bare")
        .arg(&repo_path)
        .output()
        .map_err(|e| {
            tracing::error!(error = %e, "failed to execute git init --bare");
            ApiError::Internal("failed to initialize repository storage".to_string())
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(stderr = %stderr, "git init --bare failed");
        return Err(ApiError::Internal(
            "failed to initialize repository storage".to_string(),
        ));
    }

    tracing::info!(
        repo_id = %repo_id,
        path = %repo_path.display(),
        "initialized bare git repository"
    );

    Ok(())
}

/// Emit an audit event to the `audit_events` table.
///
/// This is a fire-and-forget helper; errors are logged but do not
/// propagate to the caller so that audit failures never block the
/// primary operation.
pub async fn emit_audit_event(
    pool: &PgPool,
    actor_id: Uuid,
    event_type: &str,
    repo_id: Option<Uuid>,
    target_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO audit_events (actor_id, event_type, repo_id, target_id, metadata)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(actor_id)
    .bind(event_type)
    .bind(repo_id)
    .bind(target_id)
    .bind(metadata)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, event_type = %event_type, "failed to emit audit event");
    }
}

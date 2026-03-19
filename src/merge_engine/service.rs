use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;
use crate::pull_requests::models::{PrStatus, PullRequest};
use crate::storage;
use crate::storage::git::GitCommand;
use crate::storage::service::{repo_path, StorageConfig};

use super::models::{ConflictCheck, MergeResult, MergeStrategy};
use super::strategies::{self, MergeError};

// ---------------------------------------------------------------------------
// Advisory lock key derivation
// ---------------------------------------------------------------------------

/// Derive a stable i64 advisory lock key from a repo_id and branch name.
///
/// PostgreSQL `pg_advisory_xact_lock` takes a single bigint (i64) argument.
/// We hash the repo ID and branch name together to produce a deterministic
/// key that serializes concurrent merges targeting the same branch.
fn advisory_lock_key(repo_id: Uuid, target_branch: &str) -> i64 {
    let mut hasher = DefaultHasher::new();
    repo_id.hash(&mut hasher);
    target_branch.hash(&mut hasher);
    // Cast to i64; wrapping is fine for advisory lock keys.
    hasher.finish() as i64
}

// ---------------------------------------------------------------------------
// Branch helpers
// ---------------------------------------------------------------------------

/// Check whether a branch ref exists and return its HEAD SHA if it does.
async fn resolve_branch_sha(git: &GitCommand, branch: &str) -> Result<Option<String>, ApiError> {
    let ref_name = format!("refs/heads/{}", branch);
    let output = git.run(&["rev-parse", "--verify", &ref_name]).await?;
    if output.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(Some(sha))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Worktree helpers
// ---------------------------------------------------------------------------

/// Create a temporary worktree for merge operations.
///
/// Returns the path to the worktree directory.
async fn create_worktree(
    git: &GitCommand,
    target_branch: &str,
    worktree_path: &Path,
) -> Result<(), ApiError> {
    let path_str = worktree_path
        .to_str()
        .ok_or_else(|| ApiError::Internal("invalid worktree path".to_string()))?;

    let ref_name = format!("refs/heads/{}", target_branch);
    let output = git.run(&["worktree", "add", path_str, &ref_name]).await?;

    if !output.success() {
        tracing::error!(
            stderr = %output.stderr,
            "failed to create worktree"
        );
        return Err(ApiError::Internal(
            "failed to create temporary worktree for merge".to_string(),
        ));
    }

    Ok(())
}

/// Remove a worktree, cleaning up both the directory and git's internal tracking.
///
/// This is best-effort; errors are logged but do not propagate.
async fn cleanup_worktree(git: &GitCommand, worktree_path: &Path) {
    let path_str = match worktree_path.to_str() {
        Some(s) => s,
        None => {
            tracing::warn!("worktree path is not valid UTF-8, skipping cleanup");
            return;
        }
    };

    // Try to remove via git worktree remove (force to handle dirty state).
    let result = git.run(&["worktree", "remove", "--force", path_str]).await;
    match result {
        Ok(output) if output.success() => {
            tracing::debug!(path = %path_str, "worktree removed successfully");
        }
        Ok(output) => {
            tracing::warn!(
                path = %path_str,
                stderr = %output.stderr,
                "git worktree remove reported failure, attempting manual cleanup"
            );
        }
        Err(e) => {
            tracing::warn!(
                path = %path_str,
                error = %e,
                "git worktree remove failed, attempting manual cleanup"
            );
        }
    }

    // As a fallback, remove the directory manually.
    if worktree_path.exists() {
        if let Err(e) = tokio::fs::remove_dir_all(worktree_path).await {
            tracing::warn!(
                path = %path_str,
                error = %e,
                "failed to remove worktree directory manually"
            );
        }
    }

    // Prune stale worktree entries.
    let _ = git.run(&["worktree", "prune"]).await;
}

/// Generate a unique worktree path for a merge operation.
fn worktree_path(bare_repo_path: &Path) -> PathBuf {
    let unique_id = Uuid::new_v4();
    bare_repo_path
        .parent()
        .unwrap_or(bare_repo_path)
        .join(format!(".merge-worktree-{}", unique_id))
}

// ---------------------------------------------------------------------------
// Core merge function
// ---------------------------------------------------------------------------

/// Execute a merge of a pull request.
///
/// ## Flow
///
/// 1. Load the PR and validate its status is `open`.
/// 2. Acquire a PostgreSQL advisory lock for the target branch.
/// 3. Validate source and target branches exist; record target HEAD SHA.
/// 4. Create a temporary worktree checked out at the target branch.
/// 5. Configure git user in worktree for commit authorship.
/// 6. Delegate to the strategy-specific merge function (in `strategies`).
/// 7. On success: update PR to merged, emit events.
/// 8. Clean up the worktree (always).
/// 9. Return the merge result.
///
/// On any failure after worktree creation, the worktree is cleaned up and
/// the PR state is NOT modified.
pub async fn merge_pr(
    pool: &PgPool,
    storage: &StorageConfig,
    pr_id: Uuid,
    strategy: MergeStrategy,
    actor_id: Uuid,
    commit_message: Option<String>,
) -> Result<MergeResult, ApiError> {
    // -----------------------------------------------------------------------
    // Step 1: Load PR and validate status
    // -----------------------------------------------------------------------
    let pr = sqlx::query_as::<_, PullRequest>(r#"SELECT * FROM pull_requests WHERE id = $1"#)
        .bind(pr_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::NotFound("pull request not found".to_string()))?;

    let repo_id = pr.repo_id;

    if pr.status != PrStatus::Open {
        return Err(ApiError::Unprocessable(format!(
            "pull request is not open (status: {})",
            pr.status
        )));
    }

    // -----------------------------------------------------------------------
    // Step 2: Acquire advisory lock within a transaction
    // -----------------------------------------------------------------------
    // We use a transaction so the advisory lock is held for the duration
    // of the merge and automatically released when the transaction ends.
    let mut tx = pool.begin().await.map_err(|e| {
        tracing::error!(error = %e, "failed to begin transaction for merge");
        ApiError::Internal("failed to begin merge transaction".to_string())
    })?;

    let lock_key = advisory_lock_key(repo_id, &pr.target_branch);

    // Try to acquire the lock without blocking.
    let lock_acquired: (bool,) = sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
        .bind(lock_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to acquire advisory lock");
            ApiError::Internal("failed to acquire merge lock".to_string())
        })?;

    if !lock_acquired.0 {
        return Err(ApiError::Conflict(
            "a merge is already in progress for this target branch".to_string(),
        ));
    }

    // -----------------------------------------------------------------------
    // Step 3: Validate branches and record target HEAD SHA
    // -----------------------------------------------------------------------
    let bare_path = repo_path(storage, repo_id);
    let git = GitCommand::new(bare_path.clone());

    let _source_sha = resolve_branch_sha(&git, &pr.source_branch)
        .await?
        .ok_or_else(|| {
            ApiError::Unprocessable(format!(
                "source branch '{}' does not exist",
                pr.source_branch
            ))
        })?;

    let _target_sha = resolve_branch_sha(&git, &pr.target_branch)
        .await?
        .ok_or_else(|| {
            ApiError::Unprocessable(format!(
                "target branch '{}' does not exist",
                pr.target_branch
            ))
        })?;

    // -----------------------------------------------------------------------
    // Step 4: Create temporary worktree
    // -----------------------------------------------------------------------
    let wt_path = worktree_path(&bare_path);

    // Ensure any stale worktree at this path is cleaned up.
    if wt_path.exists() {
        let _ = tokio::fs::remove_dir_all(&wt_path).await;
    }

    if let Err(e) = create_worktree(&git, &pr.target_branch, &wt_path).await {
        cleanup_worktree(&git, &wt_path).await;
        return Err(e);
    }

    // -----------------------------------------------------------------------
    // Step 5: Configure git user in worktree
    // -----------------------------------------------------------------------
    // Git needs user.name and user.email to create commits.
    let _ = tokio::process::Command::new("git")
        .args(["config", "user.email", "orbit-merge@localhost"])
        .current_dir(&wt_path)
        .output()
        .await;

    let _ = tokio::process::Command::new("git")
        .args(["config", "user.name", "Orbit Merge"])
        .current_dir(&wt_path)
        .output()
        .await;

    // The worktree is checked out at the target branch. The source branch
    // is accessible via the bare repo's refs. Since the worktree shares the
    // object database with the bare repo, we can reference source branch
    // commits. We add the bare repo as a "remote" so `origin/<branch>` works.
    let bare_path_str = bare_path.to_str().unwrap_or("");
    let _ = tokio::process::Command::new("git")
        .args(["remote", "add", "origin", bare_path_str])
        .current_dir(&wt_path)
        .output()
        .await;

    let _ = tokio::process::Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(&wt_path)
        .output()
        .await;

    // -----------------------------------------------------------------------
    // Step 6: Execute strategy-specific merge
    // -----------------------------------------------------------------------
    let default_message = match strategy {
        MergeStrategy::MergeCommit => format!(
            "Merge branch '{}' into '{}'",
            pr.source_branch, pr.target_branch
        ),
        MergeStrategy::Squash => format!(
            "Squash merge branch '{}' into '{}'",
            pr.source_branch, pr.target_branch
        ),
        MergeStrategy::RebaseAndMerge => format!(
            "Rebase and merge branch '{}' into '{}'",
            pr.source_branch, pr.target_branch
        ),
    };

    let msg = commit_message.unwrap_or(default_message);

    let merge_sha = match strategy {
        MergeStrategy::MergeCommit => {
            strategies::execute_merge_commit(
                &git,
                &wt_path,
                &pr.source_branch,
                &pr.target_branch,
                &msg,
            )
            .await
        }
        MergeStrategy::Squash => {
            strategies::execute_squash_merge(
                &git,
                &wt_path,
                &pr.source_branch,
                &pr.target_branch,
                &msg,
            )
            .await
        }
        MergeStrategy::RebaseAndMerge => {
            strategies::execute_rebase_and_merge(
                &git,
                &wt_path,
                &pr.source_branch,
                &pr.target_branch,
                &msg,
            )
            .await
        }
    };

    let merge_sha = match merge_sha {
        Ok(sha) => sha,
        Err(MergeError::Conflict(files)) => {
            // Clean up worktree before returning conflict error.
            cleanup_worktree(&git, &wt_path).await;
            return Err(ApiError::Conflict(format!(
                "merge conflicts detected in files: {}",
                files.join(", ")
            )));
        }
        Err(MergeError::Internal(msg)) => {
            cleanup_worktree(&git, &wt_path).await;
            tracing::error!(error = %msg, "merge operation failed");
            return Err(ApiError::Internal("merge operation failed".to_string()));
        }
    };

    // -----------------------------------------------------------------------
    // Step 7: Update PR record
    // -----------------------------------------------------------------------
    let merged_at = Utc::now();

    let update_result = sqlx::query(
        r#"
        UPDATE pull_requests
        SET status = 'merged',
            merged_at = $1,
            merged_by = $2,
            updated_at = $1
        WHERE id = $3
        "#,
    )
    .bind(merged_at)
    .bind(actor_id)
    .bind(pr.id)
    .execute(&mut *tx)
    .await;

    if let Err(e) = update_result {
        cleanup_worktree(&git, &wt_path).await;
        tracing::error!(error = %e, "failed to update PR status to merged");
        return Err(ApiError::Internal(
            "failed to update pull request status".to_string(),
        ));
    }

    // Commit the transaction (releases advisory lock).
    tx.commit().await.map_err(|e| {
        tracing::error!(error = %e, "failed to commit merge transaction");
        ApiError::Internal("failed to commit merge transaction".to_string())
    })?;

    // -----------------------------------------------------------------------
    // Step 8: Emit audit events (fire-and-forget)
    // -----------------------------------------------------------------------
    storage::emit_audit_event(
        pool,
        actor_id,
        "pr.merged",
        Some(repo_id),
        Some(pr.id),
        Some(serde_json::json!({
            "number": pr.number,
            "strategy": strategy.as_str(),
            "merge_commit_sha": merge_sha,
            "source_branch": pr.source_branch,
            "target_branch": pr.target_branch,
        })),
    )
    .await;

    storage::emit_audit_event(
        pool,
        actor_id,
        "merge.completed",
        Some(repo_id),
        Some(pr.id),
        Some(serde_json::json!({
            "number": pr.number,
            "strategy": strategy.as_str(),
            "merge_commit_sha": merge_sha,
        })),
    )
    .await;

    // -----------------------------------------------------------------------
    // Step 9: Clean up worktree
    // -----------------------------------------------------------------------
    cleanup_worktree(&git, &wt_path).await;

    // -----------------------------------------------------------------------
    // Return result
    // -----------------------------------------------------------------------
    Ok(MergeResult {
        merge_commit_sha: merge_sha,
        strategy,
        merged_at,
    })
}

// ---------------------------------------------------------------------------
// Conflict check (public helper)
// ---------------------------------------------------------------------------

/// Check for merge conflicts between two branches without performing the merge.
///
/// Uses `git merge-tree` to do a trial merge. Returns a `ConflictCheck`
/// indicating whether conflicts exist and which files are affected.
pub async fn check_conflicts(
    storage: &StorageConfig,
    repo_id: Uuid,
    source_branch: &str,
    target_branch: &str,
) -> Result<ConflictCheck, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    let source_ref = format!("refs/heads/{}", source_branch);
    let target_ref = format!("refs/heads/{}", target_branch);

    // Verify both branches exist.
    let source_check = git.run(&["rev-parse", "--verify", &source_ref]).await?;
    if !source_check.success() {
        return Err(ApiError::Unprocessable(format!(
            "source branch '{}' does not exist",
            source_branch
        )));
    }

    let target_check = git.run(&["rev-parse", "--verify", &target_ref]).await?;
    if !target_check.success() {
        return Err(ApiError::Unprocessable(format!(
            "target branch '{}' does not exist",
            target_branch
        )));
    }

    // Find the merge base.
    let merge_base_output = git.run(&["merge-base", &target_ref, &source_ref]).await?;

    if !merge_base_output.success() {
        // No common ancestor -- treat as conflicting.
        return Ok(ConflictCheck {
            has_conflicts: true,
            conflicting_files: vec![],
        });
    }

    let merge_base = String::from_utf8_lossy(&merge_base_output.stdout)
        .trim()
        .to_string();

    // Use classic three-argument merge-tree.
    let merge_output = git
        .run(&["merge-tree", &merge_base, &target_ref, &source_ref])
        .await?;

    let stdout = String::from_utf8_lossy(&merge_output.stdout);

    if stdout.contains("<<<<<<") || stdout.contains("changed in both") {
        // Parse conflicting file names from merge-tree output.
        // The output includes lines like "changed in both" followed by filenames.
        let mut conflicting_files = Vec::new();
        let lines: Vec<&str> = stdout.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("changed in both") {
                // The file name is typically on the next line or in the same section.
                // Look for lines that look like file paths.
                if let Some(next_line) = lines.get(i + 1) {
                    let trimmed = next_line.trim();
                    if !trimmed.is_empty()
                        && !trimmed.starts_with('<')
                        && !trimmed.starts_with('=')
                        && !trimmed.starts_with('>')
                    {
                        conflicting_files.push(trimmed.to_string());
                    }
                }
            }
        }

        Ok(ConflictCheck {
            has_conflicts: true,
            conflicting_files,
        })
    } else {
        Ok(ConflictCheck {
            has_conflicts: false,
            conflicting_files: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_lock_key_is_deterministic() {
        let repo_id = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let key1 = advisory_lock_key(repo_id, "main");
        let key2 = advisory_lock_key(repo_id, "main");
        assert_eq!(key1, key2);
    }

    #[test]
    fn advisory_lock_key_differs_for_different_branches() {
        let repo_id = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let key1 = advisory_lock_key(repo_id, "main");
        let key2 = advisory_lock_key(repo_id, "develop");
        assert_ne!(key1, key2);
    }

    #[test]
    fn advisory_lock_key_differs_for_different_repos() {
        let repo_id1 = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let repo_id2 = Uuid::parse_str("b2c3d4e5-f6a7-8901-bcde-f12345678901").unwrap();
        let key1 = advisory_lock_key(repo_id1, "main");
        let key2 = advisory_lock_key(repo_id2, "main");
        assert_ne!(key1, key2);
    }

    #[test]
    fn worktree_path_is_sibling_of_bare_repo() {
        let bare = PathBuf::from("/data/repos/ab/abcdef.git");
        let wt = worktree_path(&bare);
        // Worktree should be in the same parent directory as the bare repo.
        assert_eq!(wt.parent().unwrap(), bare.parent().unwrap());
        // Worktree name should start with ".merge-worktree-".
        let name = wt.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(".merge-worktree-"),
            "expected worktree name starting with '.merge-worktree-', got: {}",
            name
        );
    }

    #[test]
    fn worktree_path_is_unique() {
        let bare = PathBuf::from("/data/repos/ab/abcdef.git");
        let wt1 = worktree_path(&bare);
        let wt2 = worktree_path(&bare);
        assert_ne!(wt1, wt2, "each worktree path should be unique");
    }
}

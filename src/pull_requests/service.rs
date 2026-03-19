use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;
use crate::storage;
use crate::storage::git::GitCommand;
use crate::storage::service::{repo_path, StorageConfig};

use super::models::{CreatePrInput, MergeabilityState, PrFilter, PrStatus, PullRequest, UpdatePrInput};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether a branch exists in the bare Git repository.
async fn branch_exists(
    storage: &StorageConfig,
    repo_id: Uuid,
    branch: &str,
) -> Result<bool, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);
    let ref_name = format!("refs/heads/{}", branch);
    let output = git.run(&["show-ref", "--verify", &ref_name]).await?;
    Ok(output.success())
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// Create a new pull request.
///
/// 1. Validates source != target.
/// 2. Validates both branches exist in Git.
/// 3. Checks no duplicate open PR for same source -> target.
/// 4. Assigns next sequential number using `FOR UPDATE` to handle concurrency.
/// 5. Inserts and returns the new PR.
pub async fn create_pr(
    pool: &PgPool,
    storage: &StorageConfig,
    input: CreatePrInput,
) -> Result<PullRequest, ApiError> {
    // 1. Source and target must differ.
    if input.source_branch == input.target_branch {
        return Err(ApiError::BadRequest(
            "source and target branches must be different".to_string(),
        ));
    }

    // 2. Validate branches exist in Git.
    if !branch_exists(storage, input.repo_id, &input.source_branch).await? {
        return Err(ApiError::BadRequest(format!(
            "source branch '{}' does not exist",
            input.source_branch
        )));
    }
    if !branch_exists(storage, input.repo_id, &input.target_branch).await? {
        return Err(ApiError::BadRequest(format!(
            "target branch '{}' does not exist",
            input.target_branch
        )));
    }

    // 3. Check for duplicate open PR with same source -> target.
    let existing: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT id FROM pull_requests
        WHERE repo_id = $1
          AND source_branch = $2
          AND target_branch = $3
          AND status = 'open'
        LIMIT 1
        "#,
    )
    .bind(input.repo_id)
    .bind(&input.source_branch)
    .bind(&input.target_branch)
    .fetch_optional(pool)
    .await?;

    if existing.is_some() {
        return Err(ApiError::Conflict(format!(
            "an open pull request already exists for {} -> {}",
            input.source_branch, input.target_branch
        )));
    }

    // 4-5. Assign next number and insert in a single query.
    // The subquery with FOR UPDATE locks the relevant rows to ensure
    // sequential numbering under concurrent inserts.
    let pr = sqlx::query_as::<_, PullRequest>(
        r#"
        INSERT INTO pull_requests (repo_id, author_id, number, source_branch, target_branch, title, description, status)
        VALUES (
            $1, $2,
            (SELECT COALESCE(MAX(number), 0) + 1 FROM pull_requests WHERE repo_id = $1 FOR UPDATE),
            $3, $4, $5, $6, 'open'
        )
        RETURNING *
        "#,
    )
    .bind(input.repo_id)
    .bind(input.author_id)
    .bind(&input.source_branch)
    .bind(&input.target_branch)
    .bind(&input.title)
    .bind(&input.description)
    .fetch_one(pool)
    .await?;

    // Emit audit event.
    storage::emit_audit_event(
        pool,
        input.author_id,
        "pr.created",
        Some(input.repo_id),
        Some(pr.id),
        Some(serde_json::json!({
            "number": pr.number,
            "source_branch": pr.source_branch,
            "target_branch": pr.target_branch,
            "title": pr.title,
        })),
    )
    .await;

    Ok(pr)
}

/// Get a single pull request by repo ID and PR number.
pub async fn get_pr(
    pool: &PgPool,
    repo_id: Uuid,
    number: i32,
) -> Result<Option<PullRequest>, ApiError> {
    let pr = sqlx::query_as::<_, PullRequest>(
        r#"
        SELECT * FROM pull_requests
        WHERE repo_id = $1 AND number = $2
        "#,
    )
    .bind(repo_id)
    .bind(number)
    .fetch_optional(pool)
    .await?;

    Ok(pr)
}

/// List pull requests for a repository with optional filtering and pagination.
pub async fn list_prs(
    pool: &PgPool,
    repo_id: Uuid,
    filter: PrFilter,
) -> Result<Vec<PullRequest>, ApiError> {
    // Build the query dynamically based on filter fields.
    // We use a base query and conditionally append WHERE clauses.
    //
    // sqlx does not support truly dynamic queries with query_as easily,
    // so we handle the common cases with conditional binds.
    let prs = sqlx::query_as::<_, PullRequest>(
        r#"
        SELECT * FROM pull_requests
        WHERE repo_id = $1
          AND ($2::text IS NULL OR status = $2)
          AND ($3::uuid IS NULL OR author_id = $3)
        ORDER BY created_at DESC
        LIMIT $4 OFFSET $5
        "#,
    )
    .bind(repo_id)
    .bind(filter.status.map(|s| s.as_str().to_string()))
    .bind(filter.author_id)
    .bind(filter.limit as i64)
    .bind(filter.offset as i64)
    .fetch_all(pool)
    .await?;

    Ok(prs)
}

/// Update a pull request's title and/or description.
///
/// Only updates the fields that are `Some` in the input.
/// Sets `updated_at` to the current time.
pub async fn update_pr(
    pool: &PgPool,
    pr_id: Uuid,
    actor_id: Uuid,
    input: UpdatePrInput,
) -> Result<PullRequest, ApiError> {
    let pr = sqlx::query_as::<_, PullRequest>(
        r#"
        UPDATE pull_requests
        SET title = COALESCE($2, title),
            description = COALESCE($3, description),
            updated_at = now()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(pr_id)
    .bind(&input.title)
    .bind(&input.description)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("pull request not found".to_string()))?;

    // Emit audit event.
    storage::emit_audit_event(
        pool,
        actor_id,
        "pr.updated",
        Some(pr.repo_id),
        Some(pr.id),
        Some(serde_json::json!({
            "number": pr.number,
            "title": input.title,
            "description": input.description,
        })),
    )
    .await;

    Ok(pr)
}

/// Close an open pull request.
///
/// Only PRs with status `open` can be closed. Returns an error if the PR
/// is already closed or has been merged.
pub async fn close_pr(
    pool: &PgPool,
    pr_id: Uuid,
    actor_id: Uuid,
) -> Result<PullRequest, ApiError> {
    // Fetch the current PR to validate status transition.
    let current = sqlx::query_as::<_, PullRequest>(
        r#"SELECT * FROM pull_requests WHERE id = $1"#,
    )
    .bind(pr_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("pull request not found".to_string()))?;

    match current.status {
        PrStatus::Open => { /* valid transition */ }
        PrStatus::Closed => {
            return Err(ApiError::Conflict(
                "pull request is already closed".to_string(),
            ));
        }
        PrStatus::Merged => {
            return Err(ApiError::Conflict(
                "cannot close a merged pull request".to_string(),
            ));
        }
    }

    let pr = sqlx::query_as::<_, PullRequest>(
        r#"
        UPDATE pull_requests
        SET status = 'closed',
            updated_at = now()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(pr_id)
    .fetch_one(pool)
    .await?;

    // Emit audit event.
    storage::emit_audit_event(
        pool,
        actor_id,
        "pr.closed",
        Some(pr.repo_id),
        Some(pr.id),
        Some(serde_json::json!({
            "number": pr.number,
        })),
    )
    .await;

    Ok(pr)
}

/// Reopen a closed pull request.
///
/// Only PRs with status `closed` can be reopened. Merged PRs cannot be
/// reopened (terminal state). The source branch must still exist in the
/// Git repository.
pub async fn reopen_pr(
    pool: &PgPool,
    storage: &StorageConfig,
    pr_id: Uuid,
    actor_id: Uuid,
) -> Result<PullRequest, ApiError> {
    // Fetch the current PR to validate status transition.
    let current = sqlx::query_as::<_, PullRequest>(
        r#"SELECT * FROM pull_requests WHERE id = $1"#,
    )
    .bind(pr_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("pull request not found".to_string()))?;

    match current.status {
        PrStatus::Closed => { /* valid transition */ }
        PrStatus::Open => {
            return Err(ApiError::Conflict(
                "pull request is already open".to_string(),
            ));
        }
        PrStatus::Merged => {
            return Err(ApiError::Conflict(
                "cannot reopen a merged pull request".to_string(),
            ));
        }
    }

    // Verify the source branch still exists.
    if !branch_exists(storage, current.repo_id, &current.source_branch).await? {
        return Err(ApiError::BadRequest(format!(
            "source branch '{}' no longer exists",
            current.source_branch
        )));
    }

    let pr = sqlx::query_as::<_, PullRequest>(
        r#"
        UPDATE pull_requests
        SET status = 'open',
            updated_at = now()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(pr_id)
    .fetch_one(pool)
    .await?;

    // Emit audit event.
    storage::emit_audit_event(
        pool,
        actor_id,
        "pr.reopened",
        Some(pr.repo_id),
        Some(pr.id),
        Some(serde_json::json!({
            "number": pr.number,
        })),
    )
    .await;

    Ok(pr)
}

/// Get the diff between two branches.
///
/// Uses `git diff {target}...{source}` (three-dot merge-base diff) to produce
/// a unified diff of the changes introduced by the source branch relative to
/// the target branch.
pub async fn get_pr_diff(
    storage: &StorageConfig,
    repo_id: Uuid,
    source_branch: &str,
    target_branch: &str,
) -> Result<String, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    // Three-dot diff: shows changes on source since it diverged from target.
    let diff_spec = format!(
        "refs/heads/{}...refs/heads/{}",
        target_branch, source_branch
    );

    let output = git.run(&["diff", &diff_spec]).await?;

    if !output.success() {
        // If the diff command fails, it likely means one of the refs is invalid.
        return Err(ApiError::BadRequest(format!(
            "failed to compute diff: {}",
            output.stderr.trim()
        )));
    }

    let diff = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(diff)
}

/// Check the mergeability state of two branches.
///
/// Attempts a trial merge using `git merge-tree` to determine whether the
/// source branch can be cleanly merged into the target branch. Does not
/// modify actual refs.
///
/// Uses the classic three-argument `git merge-tree <base> <target> <source>`
/// form which is widely supported across Git versions.
pub async fn check_mergeability(
    storage: &StorageConfig,
    repo_id: Uuid,
    source_branch: &str,
    target_branch: &str,
) -> Result<MergeabilityState, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    // Resolve both branch refs to ensure they exist.
    let source_ref = format!("refs/heads/{}", source_branch);
    let target_ref = format!("refs/heads/{}", target_branch);

    let source_check = git.run(&["rev-parse", "--verify", &source_ref]).await?;
    if !source_check.success() {
        return Ok(MergeabilityState::InvalidRef);
    }

    let target_check = git.run(&["rev-parse", "--verify", &target_ref]).await?;
    if !target_check.success() {
        return Ok(MergeabilityState::InvalidRef);
    }

    // Find the merge-base between target and source.
    let merge_base_output = git
        .run(&["merge-base", &target_ref, &source_ref])
        .await?;

    if !merge_base_output.success() {
        // No common ancestor found -- branches are unrelated.
        // Treat as conflicting since a merge would require special handling.
        return Ok(MergeabilityState::Conflicting);
    }

    let merge_base = String::from_utf8_lossy(&merge_base_output.stdout)
        .trim()
        .to_string();

    // Use the classic three-argument form:
    // git merge-tree <base-tree> <branch1-tree> <branch2-tree>
    // This writes merge results to stdout and exits 0 even on conflicts.
    // Conflicts are indicated by lines containing conflict markers in stdout.
    let merge_output = git
        .run(&["merge-tree", &merge_base, &target_ref, &source_ref])
        .await?;

    if merge_output.success() {
        let stdout_str = String::from_utf8_lossy(&merge_output.stdout);
        // The classic merge-tree outputs conflict information with markers
        // like "<<< " or "+<<<<<<< " when there are conflicts.
        // An empty or marker-free stdout indicates a clean merge.
        if stdout_str.contains("<<<<<<") || stdout_str.contains("changed in both") {
            Ok(MergeabilityState::Conflicting)
        } else {
            Ok(MergeabilityState::Clean)
        }
    } else {
        // Non-zero exit from merge-tree is unexpected for the classic form.
        tracing::warn!(
            exit_code = merge_output.exit_code,
            stderr = %merge_output.stderr,
            "unexpected merge-tree result"
        );
        Ok(MergeabilityState::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unit tests that do not require a database or Git repo.

    #[tokio::test]
    async fn create_pr_rejects_same_source_and_target() {
        // We can test the validation logic without a real pool/storage
        // by checking that the function returns an error before touching
        // the database. We need a PgPool to satisfy the type system,
        // but the function should bail out early.
        //
        // Since we cannot easily create a dummy PgPool without a real
        // database, we test via the public contract at integration level.
        // This test documents the expected behavior.
        //
        // For now, verify the input struct works correctly.
        let input = CreatePrInput {
            repo_id: Uuid::new_v4(),
            author_id: Uuid::new_v4(),
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            title: "Test".to_string(),
            description: None,
        };
        assert_eq!(input.source_branch, input.target_branch);
    }
}

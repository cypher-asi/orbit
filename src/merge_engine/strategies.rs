use std::path::Path;

use crate::storage::git::GitCommand;

// ---------------------------------------------------------------------------
// Internal error type for merge operations
// ---------------------------------------------------------------------------

/// Internal error type for strategy-specific merge operations.
///
/// Separates conflicts from other failures so the caller can map them
/// to appropriate API responses.
#[derive(Debug)]
pub(crate) enum MergeError {
    /// Merge has conflicts; includes list of conflicting files.
    Conflict(Vec<String>),
    /// Internal/unexpected error.
    Internal(String),
}

// ---------------------------------------------------------------------------
// Worktree command helper
// ---------------------------------------------------------------------------

/// Run a Git command inside a worktree directory, returning the raw output.
///
/// Unlike `GitCommand::run` (which sets `GIT_DIR` to the bare repo), this
/// runs git with `current_dir` set to the worktree so that normal (non-bare)
/// git operations work correctly.
async fn git_in_worktree(
    worktree_path: &Path,
    args: &[&str],
) -> Result<std::process::Output, MergeError> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(worktree_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|e| MergeError::Internal(format!("failed to execute git {}: {}", args[0], e)))
}

/// Run a Git command inside a worktree, returning a trimmed stdout string on
/// success or a `MergeError` if the command fails.
async fn git_in_worktree_ok(
    worktree_path: &Path,
    args: &[&str],
    error_context: &str,
) -> Result<String, MergeError> {
    let output = git_in_worktree(worktree_path, args).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MergeError::Internal(format!("{}: {}", error_context, stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the current HEAD SHA inside a worktree.
async fn head_sha(worktree_path: &Path) -> Result<String, MergeError> {
    git_in_worktree_ok(worktree_path, &["rev-parse", "HEAD"], "failed to get HEAD SHA").await
}

// ---------------------------------------------------------------------------
// Conflict detection helpers
// ---------------------------------------------------------------------------

/// Check whether git command output indicates merge conflicts.
fn output_has_conflicts(output: &std::process::Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    stderr.contains("CONFLICT")
        || stdout.contains("CONFLICT")
        || stderr.contains("Automatic merge failed")
        || stdout.contains("Automatic merge failed")
}

/// Parse conflict file names from the worktree by checking
/// `git diff --name-only --diff-filter=U`.
async fn parse_conflict_files(worktree_path: &Path) -> Vec<String> {
    let output = git_in_worktree(worktree_path, &["diff", "--name-only", "--diff-filter=U"]).await;

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Merge commit strategy
// ---------------------------------------------------------------------------

/// Execute a merge commit strategy in the worktree.
///
/// In the worktree (already checked out at `target`):
/// 1. Detach HEAD (so the bare repo ref is not moved by the merge).
/// 2. `git merge --no-ff origin/{source} -m {message}`
/// 3. Atomically update the bare repo ref via `git update-ref`.
/// 4. Return the merge commit SHA.
///
/// Creates a merge commit with two parents (target HEAD + source HEAD).
pub(crate) async fn execute_merge_commit(
    git: &GitCommand,
    worktree_path: &Path,
    source: &str,
    target: &str,
    message: &str,
) -> Result<String, MergeError> {
    let source_ref = format!("origin/{}", source);

    // Ensure we are on the target branch, then record the old SHA.
    let _ = git_in_worktree_ok(
        worktree_path,
        &["checkout", target],
        "failed to checkout target branch",
    )
    .await?;

    let old_sha = head_sha(worktree_path).await?;

    // Detach HEAD so the merge does not move the bare repo's branch ref.
    let _ = git_in_worktree_ok(
        worktree_path,
        &["checkout", "--detach"],
        "failed to detach HEAD",
    )
    .await?;

    // Perform the merge.
    let output = git_in_worktree(
        worktree_path,
        &["merge", "--no-ff", &source_ref, "-m", message],
    )
    .await?;

    if !output.status.success() {
        if output_has_conflicts(&output) {
            let files = parse_conflict_files(worktree_path).await;
            return Err(MergeError::Conflict(files));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MergeError::Internal(format!("git merge failed: {}", stderr)));
    }

    // Get the resulting merge commit SHA.
    let new_sha = head_sha(worktree_path).await?;

    // Update the bare repo ref atomically.
    update_target_ref(git, target, &new_sha, &old_sha).await?;

    Ok(new_sha)
}

// ---------------------------------------------------------------------------
// Squash merge strategy
// ---------------------------------------------------------------------------

/// Execute a squash merge strategy in the worktree.
///
/// In the worktree (already checked out at `target`):
/// 1. Detach HEAD (so the bare repo ref is not moved by the commit).
/// 2. `git merge --squash origin/{source}`
/// 3. `git commit -m {message}`
/// 4. Atomically update the bare repo ref via `git update-ref`.
/// 5. Return the commit SHA.
///
/// Creates a single commit on the target branch with all source changes.
pub(crate) async fn execute_squash_merge(
    git: &GitCommand,
    worktree_path: &Path,
    source: &str,
    target: &str,
    message: &str,
) -> Result<String, MergeError> {
    let source_ref = format!("origin/{}", source);

    // Ensure we are on the target branch, then record the old SHA.
    let _ = git_in_worktree_ok(
        worktree_path,
        &["checkout", target],
        "failed to checkout target branch",
    )
    .await?;

    let old_sha = head_sha(worktree_path).await?;

    // Detach HEAD so the commit does not move the bare repo's branch ref.
    let _ = git_in_worktree_ok(
        worktree_path,
        &["checkout", "--detach"],
        "failed to detach HEAD",
    )
    .await?;

    // Squash merge.
    let output = git_in_worktree(
        worktree_path,
        &["merge", "--squash", &source_ref],
    )
    .await?;

    if !output.status.success() {
        if output_has_conflicts(&output) {
            let files = parse_conflict_files(worktree_path).await;
            return Err(MergeError::Conflict(files));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MergeError::Internal(format!(
            "git merge --squash failed: {}",
            stderr
        )));
    }

    // Commit the squashed changes.
    let commit_output = git_in_worktree(
        worktree_path,
        &["commit", "-m", message],
    )
    .await?;

    if !commit_output.status.success() {
        let stderr = String::from_utf8_lossy(&commit_output.stderr);
        return Err(MergeError::Internal(format!(
            "git commit after squash failed: {}",
            stderr
        )));
    }

    // Get the resulting commit SHA.
    let new_sha = head_sha(worktree_path).await?;

    // Update the bare repo ref atomically.
    update_target_ref(git, target, &new_sha, &old_sha).await?;

    Ok(new_sha)
}

// ---------------------------------------------------------------------------
// Rebase and merge strategy
// ---------------------------------------------------------------------------

/// Execute a rebase-and-merge strategy in the worktree.
///
/// 1. Checkout `target`, record old SHA, then detach HEAD.
/// 2. `git rebase HEAD origin/{source}` -- rebases source commits onto target tip.
///    (After rebase, HEAD is at the rebased tip in detached state.)
/// 3. Capture the rebased tip SHA.
/// 4. Atomically update the bare repo ref via `git update-ref`.
/// 5. Return the new HEAD SHA.
///
/// Rebases source commits onto the target branch tip, then fast-forwards
/// the target branch pointer via the atomic ref update.
pub(crate) async fn execute_rebase_and_merge(
    git: &GitCommand,
    worktree_path: &Path,
    source: &str,
    target: &str,
    _message: &str,
) -> Result<String, MergeError> {
    let source_ref = format!("origin/{}", source);

    // Ensure we start on the target branch to record old SHA.
    let _ = git_in_worktree_ok(
        worktree_path,
        &["checkout", target],
        "failed to checkout target branch",
    )
    .await?;

    let old_sha = head_sha(worktree_path).await?;

    // Rebase source onto target: git rebase {target} {source_ref}
    // This checks out source_ref and replays its commits onto target.
    // After completion, HEAD is at the rebased tip (detached).
    // Note: since we want to keep target ref untouched, and git rebase
    // with a remote ref naturally ends in detached HEAD, this is safe.
    let rebase_output = git_in_worktree(
        worktree_path,
        &["rebase", target, &source_ref],
    )
    .await?;

    if !rebase_output.status.success() {
        let stderr = String::from_utf8_lossy(&rebase_output.stderr);
        let stdout = String::from_utf8_lossy(&rebase_output.stdout);

        if stderr.contains("CONFLICT")
            || stdout.contains("CONFLICT")
            || stderr.contains("could not apply")
        {
            // Abort the rebase to leave worktree in a clean state.
            let _ = git_in_worktree(worktree_path, &["rebase", "--abort"]).await;
            let files = parse_conflict_files(worktree_path).await;
            return Err(MergeError::Conflict(files));
        }

        return Err(MergeError::Internal(format!(
            "git rebase failed: {}",
            stderr
        )));
    }

    // After rebase, HEAD is at the rebased tip (detached HEAD).
    let new_sha = head_sha(worktree_path).await?;

    // Update the bare repo ref atomically. This effectively fast-forwards
    // the target branch to the rebased tip.
    update_target_ref(git, target, &new_sha, &old_sha).await?;

    Ok(new_sha)
}

// ---------------------------------------------------------------------------
// Atomic ref update
// ---------------------------------------------------------------------------

/// Update the bare repo's target branch ref atomically.
///
/// Runs: `git update-ref refs/heads/{target} {new_sha} {old_sha}`
///
/// The old SHA check ensures that the ref hasn't been moved by another
/// concurrent operation since we recorded it.
pub(crate) async fn update_target_ref(
    git: &GitCommand,
    target: &str,
    new_sha: &str,
    old_sha: &str,
) -> Result<(), MergeError> {
    let target_ref = format!("refs/heads/{}", target);
    let output = git
        .run(&["update-ref", &target_ref, new_sha, old_sha])
        .await
        .map_err(|e| {
            MergeError::Internal(format!("failed to run git update-ref: {}", e))
        })?;

    if !output.success() {
        return Err(MergeError::Internal(format!(
            "git update-ref failed: {}",
            output.stderr
        )));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Helper: create a non-bare repo with an initial commit on "main" and
    // return a (tempdir, repo_path) tuple. The tempdir handle keeps the
    // directory alive.
    async fn init_test_repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo.git");

        // Init bare repo with "main" as default branch
        let out = tokio::process::Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&repo)
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "git init --bare failed");

        // Create a working clone so we can make commits
        let clone_dir = tmp.path().join("clone");
        let out = tokio::process::Command::new("git")
            .args(["clone", repo.to_str().unwrap(), clone_dir.to_str().unwrap()])
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "git clone failed");

        // Configure user
        for args in [
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test User"],
        ] {
            let out = tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&clone_dir)
                .output()
                .await
                .unwrap();
            assert!(out.status.success());
        }

        // Ensure we are on the "main" branch
        let _ = tokio::process::Command::new("git")
            .args(["checkout", "-b", "main"])
            .current_dir(&clone_dir)
            .output()
            .await;

        // Create initial commit on main
        let readme = clone_dir.join("README.md");
        tokio::fs::write(&readme, "# Test Repo\n").await.unwrap();

        let out = tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["push", "origin", "HEAD:refs/heads/main"])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "push main failed: {}", String::from_utf8_lossy(&out.stderr));

        (tmp, repo)
    }

    // Helper: create a feature branch with a commit, push it, and return
    // the clone directory for further operations.
    async fn create_feature_branch(
        tmp: &tempfile::TempDir,
        bare_path: &Path,
        branch_name: &str,
        filename: &str,
        content: &str,
    ) -> PathBuf {
        let clone_dir = tmp.path().join(format!("clone-{}", branch_name));
        let out = tokio::process::Command::new("git")
            .args(["clone", "-b", "main", bare_path.to_str().unwrap(), clone_dir.to_str().unwrap()])
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "clone failed: {}", String::from_utf8_lossy(&out.stderr));

        // Configure user
        for args in [
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test User"],
        ] {
            let out = tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&clone_dir)
                .output()
                .await
                .unwrap();
            assert!(out.status.success());
        }

        // Create and checkout branch
        let out = tokio::process::Command::new("git")
            .args(["checkout", "-b", branch_name])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        // Add a file and commit
        let file_path = clone_dir.join(filename);
        tokio::fs::write(&file_path, content).await.unwrap();

        let out = tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["commit", "-m", &format!("add {} on {}", filename, branch_name)])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["push", "origin", branch_name])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "push {} failed: {}",
            branch_name,
            String::from_utf8_lossy(&out.stderr)
        );

        clone_dir
    }

    // Helper: create a worktree from the bare repo for the target branch,
    // configure user + remote, and fetch.
    async fn setup_worktree(
        bare_path: &Path,
        target_branch: &str,
    ) -> (tempfile::TempDir, PathBuf) {
        let wt_tmp = tempfile::tempdir().expect("tempdir for worktree");
        let wt_path = wt_tmp.path().join("worktree");

        let ref_name = format!("refs/heads/{}", target_branch);
        let out = tokio::process::Command::new("git")
            .args(["worktree", "add", wt_path.to_str().unwrap(), &ref_name])
            .env("GIT_DIR", bare_path)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Configure user in worktree
        for args in [
            vec!["config", "user.email", "merge@orbit.local"],
            vec!["config", "user.name", "Orbit Merge"],
        ] {
            let out = tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&wt_path)
                .output()
                .await
                .unwrap();
            assert!(out.status.success());
        }

        // Add bare repo as remote "origin" and fetch
        let out = tokio::process::Command::new("git")
            .args(["remote", "add", "origin", bare_path.to_str().unwrap()])
            .current_dir(&wt_path)
            .output()
            .await
            .unwrap();
        // Might fail if remote already exists; that's ok
        let _ = out;

        let out = tokio::process::Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&wt_path)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "fetch failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        (wt_tmp, wt_path)
    }

    // Helper: get a ref SHA from the bare repo.
    async fn get_ref_sha(bare_path: &Path, branch: &str) -> String {
        let git = GitCommand::new(bare_path.to_path_buf());
        let ref_name = format!("refs/heads/{}", branch);
        let output = git.run(&["rev-parse", &ref_name]).await.unwrap();
        assert!(output.success());
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    // Helper: count parents of a commit in the bare repo.
    async fn commit_parent_count(bare_path: &Path, sha: &str) -> usize {
        let git = GitCommand::new(bare_path.to_path_buf());
        let output = git.run(&["cat-file", "-p", sha]).await.unwrap();
        assert!(output.success());
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().filter(|l| l.starts_with("parent ")).count()
    }

    // -----------------------------------------------------------------------
    // Merge commit tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn merge_commit_creates_two_parent_commit() {
        let (tmp, bare) = init_test_repo().await;

        // Create a feature branch with a new file
        let _ = create_feature_branch(&tmp, &bare, "feature-mc", "feature.txt", "hello\n").await;

        let git = GitCommand::new(bare.clone());

        // Record SHAs before merge
        let main_sha_before = get_ref_sha(&bare, "main").await;
        let feature_sha = get_ref_sha(&bare, "feature-mc").await;

        // Set up worktree
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_merge_commit(
            &git,
            &wt_path,
            "feature-mc",
            "main",
            "Merge feature-mc into main",
        )
        .await;

        assert!(result.is_ok(), "merge_commit failed: {:?}", result.err().map(|e| match e {
            MergeError::Internal(s) => s,
            MergeError::Conflict(f) => format!("conflicts: {:?}", f),
        }));

        let merge_sha = result.unwrap();
        assert!(!merge_sha.is_empty());
        assert_ne!(merge_sha, main_sha_before);

        // Verify the merge commit has two parents
        let parents = commit_parent_count(&bare, &merge_sha).await;
        assert_eq!(parents, 2, "merge commit should have exactly 2 parents");

        // Verify the bare repo's main ref was updated
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_after, merge_sha);

        // Verify one parent is the old main and the other is the feature branch
        let git2 = GitCommand::new(bare.clone());
        let output = git2.run(&["cat-file", "-p", &merge_sha]).await.unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let parent_shas: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("parent "))
            .map(|l| l.strip_prefix("parent ").unwrap().trim())
            .collect();
        assert!(parent_shas.contains(&main_sha_before.as_str()));
        assert!(parent_shas.contains(&feature_sha.as_str()));
    }

    #[tokio::test]
    async fn merge_commit_uses_custom_message() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(&tmp, &bare, "feature-msg", "msg.txt", "content\n").await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let merge_sha = execute_merge_commit(
            &git,
            &wt_path,
            "feature-msg",
            "main",
            "Custom merge message",
        )
        .await
        .unwrap();

        // Verify commit message
        let git2 = GitCommand::new(bare.clone());
        let output = git2.run(&["log", "-1", "--format=%s", &merge_sha]).await.unwrap();
        let subject = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(subject, "Custom merge message");
    }

    // -----------------------------------------------------------------------
    // Squash merge tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn squash_merge_creates_single_commit() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(&tmp, &bare, "feature-sq", "squash.txt", "squashed\n").await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_squash_merge(
            &git,
            &wt_path,
            "feature-sq",
            "main",
            "Squash feature-sq into main",
        )
        .await;

        assert!(result.is_ok(), "squash_merge failed: {:?}", result.err().map(|e| match e {
            MergeError::Internal(s) => s,
            MergeError::Conflict(f) => format!("conflicts: {:?}", f),
        }));

        let squash_sha = result.unwrap();
        assert!(!squash_sha.is_empty());
        assert_ne!(squash_sha, main_sha_before);

        // Squash commit should have exactly ONE parent (no merge commit)
        let parents = commit_parent_count(&bare, &squash_sha).await;
        assert_eq!(parents, 1, "squash commit should have exactly 1 parent");

        // The single parent should be the old main
        let git2 = GitCommand::new(bare.clone());
        let output = git2.run(&["cat-file", "-p", &squash_sha]).await.unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let parent_sha: &str = text
            .lines()
            .find(|l| l.starts_with("parent "))
            .unwrap()
            .strip_prefix("parent ")
            .unwrap()
            .trim();
        assert_eq!(parent_sha, main_sha_before);

        // Verify the bare repo ref was updated
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_after, squash_sha);
    }

    #[tokio::test]
    async fn squash_merge_uses_custom_message() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(&tmp, &bare, "feature-sqm", "sqm.txt", "content\n").await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let squash_sha = execute_squash_merge(
            &git,
            &wt_path,
            "feature-sqm",
            "main",
            "Squash merge message",
        )
        .await
        .unwrap();

        let git2 = GitCommand::new(bare.clone());
        let output = git2.run(&["log", "-1", "--format=%s", &squash_sha]).await.unwrap();
        let subject = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(subject, "Squash merge message");
    }

    // -----------------------------------------------------------------------
    // Rebase and merge tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn rebase_and_merge_fast_forwards_target() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(&tmp, &bare, "feature-rb", "rebase.txt", "rebased\n").await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_rebase_and_merge(
            &git,
            &wt_path,
            "feature-rb",
            "main",
            "Rebase and merge",
        )
        .await;

        assert!(result.is_ok(), "rebase_and_merge failed: {:?}", result.err().map(|e| match e {
            MergeError::Internal(s) => s,
            MergeError::Conflict(f) => format!("conflicts: {:?}", f),
        }));

        let new_sha = result.unwrap();
        assert!(!new_sha.is_empty());
        assert_ne!(new_sha, main_sha_before);

        // The result should be a single-parent commit (no merge commit).
        let parents = commit_parent_count(&bare, &new_sha).await;
        assert_eq!(parents, 1, "rebased commit should have exactly 1 parent");

        // Verify the bare repo ref was updated
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_after, new_sha);

        // The parent of the new commit should be the old main HEAD
        // (since the feature branch was based on main and rebased onto main).
        let git2 = GitCommand::new(bare.clone());
        let output = git2.run(&["cat-file", "-p", &new_sha]).await.unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let parent_sha: &str = text
            .lines()
            .find(|l| l.starts_with("parent "))
            .unwrap()
            .strip_prefix("parent ")
            .unwrap()
            .trim();
        assert_eq!(parent_sha, main_sha_before);
    }

    // -----------------------------------------------------------------------
    // Conflict detection tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn merge_commit_detects_conflicts() {
        let (tmp, bare) = init_test_repo().await;

        // Create two branches that modify the same file differently
        let _clone1 = create_feature_branch(
            &tmp, &bare, "conflict-a", "shared.txt", "content from branch a\n",
        )
        .await;

        // We need to push a change to main that conflicts with conflict-a.
        // First update main with a conflicting file.
        let clone_main = tmp.path().join("clone-main-conflict");
        let out = tokio::process::Command::new("git")
            .args(["clone", "-b", "main", bare.to_str().unwrap(), clone_main.to_str().unwrap()])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        for args in [
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test User"],
        ] {
            let out = tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&clone_main)
                .output()
                .await
                .unwrap();
            assert!(out.status.success());
        }

        tokio::fs::write(clone_main.join("shared.txt"), "content from main\n")
            .await
            .unwrap();

        let out = tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&clone_main)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["commit", "-m", "conflicting change on main"])
            .current_dir(&clone_main)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&clone_main)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        // Now try to merge conflict-a into main
        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_merge_commit(
            &git,
            &wt_path,
            "conflict-a",
            "main",
            "This should fail",
        )
        .await;

        match result {
            Err(MergeError::Conflict(files)) => {
                assert!(
                    files.contains(&"shared.txt".to_string()),
                    "expected shared.txt in conflict files, got: {:?}",
                    files
                );
            }
            Err(MergeError::Internal(msg)) => {
                panic!("expected Conflict error, got Internal: {}", msg);
            }
            Ok(sha) => {
                panic!("expected conflict, but merge succeeded with sha: {}", sha);
            }
        }
    }

    #[tokio::test]
    async fn update_target_ref_updates_bare_repo() {
        let (_tmp, bare) = init_test_repo().await;
        let git = GitCommand::new(bare.clone());

        let old_sha = get_ref_sha(&bare, "main").await;

        // Create a new commit object to point the ref at.
        // We'll use commit-tree for this.
        let tree_output = git.run(&["rev-parse", "main^{tree}"]).await.unwrap();
        assert!(tree_output.success());
        let tree_sha = String::from_utf8_lossy(&tree_output.stdout).trim().to_string();

        let commit_output = git
            .run(&["commit-tree", &tree_sha, "-p", &old_sha, "-m", "test commit"])
            .await
            .unwrap();
        assert!(commit_output.success());
        let new_sha = String::from_utf8_lossy(&commit_output.stdout).trim().to_string();

        // Use our helper
        let result = update_target_ref(&git, "main", &new_sha, &old_sha).await;
        assert!(result.is_ok());

        let updated_sha = get_ref_sha(&bare, "main").await;
        assert_eq!(updated_sha, new_sha);
    }

    #[tokio::test]
    async fn update_target_ref_fails_on_wrong_old_sha() {
        let (_tmp, bare) = init_test_repo().await;
        let git = GitCommand::new(bare.clone());

        let real_sha = get_ref_sha(&bare, "main").await;

        // Create a new commit
        let tree_output = git.run(&["rev-parse", "main^{tree}"]).await.unwrap();
        let tree_sha = String::from_utf8_lossy(&tree_output.stdout).trim().to_string();

        let commit_output = git
            .run(&["commit-tree", &tree_sha, "-p", &real_sha, "-m", "test"])
            .await
            .unwrap();
        let new_sha = String::from_utf8_lossy(&commit_output.stdout).trim().to_string();

        // Try to update with a wrong old SHA
        let wrong_old = "0000000000000000000000000000000000000000";
        let result = update_target_ref(&git, "main", &new_sha, wrong_old).await;
        assert!(
            matches!(result, Err(MergeError::Internal(_))),
            "expected Internal error for stale old SHA"
        );

        // Ref should not have changed
        let current_sha = get_ref_sha(&bare, "main").await;
        assert_eq!(current_sha, real_sha);
    }
}

use uuid::Uuid;

use crate::errors::ApiError;
use crate::storage::git::GitCommand;
use crate::storage::service::{repo_path, StorageConfig};

use super::models::BranchInfo;

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate that a branch name is acceptable for Git.
///
/// Rejects names that contain spaces, `..`, control characters, or other
/// patterns that are invalid or dangerous in Git ref names.
fn validate_branch_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() {
        return Err(ApiError::BadRequest(
            "branch name must not be empty".to_string(),
        ));
    }

    if name.contains(' ') {
        return Err(ApiError::BadRequest(
            "branch name must not contain spaces".to_string(),
        ));
    }

    if name.contains("..") {
        return Err(ApiError::BadRequest(
            "branch name must not contain '..'".to_string(),
        ));
    }

    if name.starts_with('-') {
        return Err(ApiError::BadRequest(
            "branch name must not start with '-'".to_string(),
        ));
    }

    if name.ends_with('/') || name.starts_with('/') {
        return Err(ApiError::BadRequest(
            "branch name must not start or end with '/'".to_string(),
        ));
    }

    if name.ends_with('.') {
        return Err(ApiError::BadRequest(
            "branch name must not end with '.'".to_string(),
        ));
    }

    if name.contains('~') || name.contains('^') || name.contains(':') || name.contains('\\') {
        return Err(ApiError::BadRequest(
            "branch name contains invalid characters".to_string(),
        ));
    }

    if name.contains('\0') || name.chars().any(|c| c.is_control()) {
        return Err(ApiError::BadRequest(
            "branch name must not contain control characters".to_string(),
        ));
    }

    if name.contains("@{") {
        return Err(ApiError::BadRequest(
            "branch name must not contain '@{'".to_string(),
        ));
    }

    if name == "@" {
        return Err(ApiError::BadRequest(
            "branch name must not be '@'".to_string(),
        ));
    }

    if name.contains("*.") || name.contains("[") {
        return Err(ApiError::BadRequest(
            "branch name contains invalid characters".to_string(),
        ));
    }

    if name.ends_with(".lock") {
        return Err(ApiError::BadRequest(
            "branch name must not end with '.lock'".to_string(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// List all branches in a repository.
///
/// The `default_branch` parameter is used to mark which branch is the default.
/// Runs `git for-each-ref --format='%(refname:short) %(objectname)' refs/heads/`
/// and parses the output.
pub async fn list_branches(
    storage: &StorageConfig,
    repo_id: Uuid,
    default_branch: &str,
) -> Result<Vec<BranchInfo>, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    let output = git
        .run(&[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads/",
        ])
        .await?;

    if !output.success() {
        // If the repo is empty (no commits yet) for-each-ref may succeed
        // with empty output or fail. Either way, return an empty list.
        tracing::warn!(
            repo_id = %repo_id,
            stderr = %output.stderr,
            "git for-each-ref failed or returned non-zero"
        );
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branches = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Format: "branch_name commit_sha"
        // The branch name may contain slashes (e.g. "feature/foo"),
        // so we split from the right on the last space.
        if let Some(last_space) = line.rfind(' ') {
            let name = &line[..last_space];
            let sha = &line[last_space + 1..];
            branches.push(BranchInfo {
                name: name.to_string(),
                head_commit: sha.to_string(),
                is_default: name == default_branch,
            });
        }
    }

    Ok(branches)
}

/// Get information about a single branch.
///
/// Runs `git show-ref --verify refs/heads/{name}` to check existence and
/// get the commit SHA. Returns `None` if the branch does not exist.
pub async fn get_branch(
    storage: &StorageConfig,
    repo_id: Uuid,
    name: &str,
    default_branch: &str,
) -> Result<Option<BranchInfo>, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    let ref_name = format!("refs/heads/{}", name);
    let output = git.run(&["show-ref", "--verify", &ref_name]).await?;

    if !output.success() {
        // Branch does not exist.
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();

    // show-ref output format: "<sha> refs/heads/<name>"
    let sha = if let Some(space_pos) = line.find(' ') {
        line[..space_pos].to_string()
    } else {
        // Unexpected format; try rev-parse as fallback.
        let rp_output = git.run(&["rev-parse", &ref_name]).await?;
        if !rp_output.success() {
            return Ok(None);
        }
        String::from_utf8_lossy(&rp_output.stdout)
            .trim()
            .to_string()
    };

    Ok(Some(BranchInfo {
        name: name.to_string(),
        head_commit: sha,
        is_default: name == default_branch,
    }))
}

/// Create a new branch at the specified start point.
///
/// Validates the branch name, checks that it does not already exist,
/// then runs `git branch {name} {start_point}`.
pub async fn create_branch(
    storage: &StorageConfig,
    repo_id: Uuid,
    name: &str,
    start_point: &str,
    default_branch: &str,
) -> Result<BranchInfo, ApiError> {
    // Validate the branch name.
    validate_branch_name(name)?;

    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    // Check if the branch already exists.
    let ref_name = format!("refs/heads/{}", name);
    let exists_output = git.run(&["show-ref", "--verify", &ref_name]).await?;
    if exists_output.success() {
        return Err(ApiError::Conflict(format!(
            "branch '{}' already exists",
            name
        )));
    }

    // Create the branch.
    let output = git.run(&["branch", name, start_point]).await?;
    if !output.success() {
        tracing::error!(
            repo_id = %repo_id,
            branch = %name,
            start_point = %start_point,
            stderr = %output.stderr,
            "git branch create failed"
        );
        return Err(ApiError::BadRequest(format!(
            "failed to create branch: {}",
            output.stderr.trim()
        )));
    }

    // Fetch the new branch's head commit.
    let rp_output = git.run(&["rev-parse", &ref_name]).await?;
    let sha = if rp_output.success() {
        String::from_utf8_lossy(&rp_output.stdout)
            .trim()
            .to_string()
    } else {
        // The branch was just created, so this should not happen.
        String::new()
    };

    Ok(BranchInfo {
        name: name.to_string(),
        head_commit: sha,
        is_default: name == default_branch,
    })
}

/// Delete a branch.
///
/// Refuses to delete the default branch. Runs `git branch -D {name}`.
pub async fn delete_branch(
    storage: &StorageConfig,
    repo_id: Uuid,
    name: &str,
    default_branch: &str,
) -> Result<(), ApiError> {
    // Cannot delete the default branch.
    if name == default_branch {
        return Err(ApiError::BadRequest(
            "cannot delete the default branch".to_string(),
        ));
    }

    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    // Verify the branch exists first.
    let ref_name = format!("refs/heads/{}", name);
    let exists_output = git.run(&["show-ref", "--verify", &ref_name]).await?;
    if !exists_output.success() {
        return Err(ApiError::NotFound(format!("branch '{}' not found", name)));
    }

    let output = git.run(&["branch", "-D", name]).await?;
    if !output.success() {
        tracing::error!(
            repo_id = %repo_id,
            branch = %name,
            stderr = %output.stderr,
            "git branch delete failed"
        );
        return Err(ApiError::Internal(format!(
            "failed to delete branch: {}",
            output.stderr.trim()
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

    // -- Validation tests ---------------------------------------------------

    #[test]
    fn validate_valid_names() {
        assert!(validate_branch_name("main").is_ok());
        assert!(validate_branch_name("feature/foo").is_ok());
        assert!(validate_branch_name("release-1.0").is_ok());
        assert!(validate_branch_name("my-branch").is_ok());
        assert!(validate_branch_name("a").is_ok());
    }

    #[test]
    fn validate_empty_name() {
        assert!(validate_branch_name("").is_err());
    }

    #[test]
    fn validate_name_with_spaces() {
        assert!(validate_branch_name("my branch").is_err());
    }

    #[test]
    fn validate_name_with_double_dot() {
        assert!(validate_branch_name("a..b").is_err());
    }

    #[test]
    fn validate_name_starting_with_dash() {
        assert!(validate_branch_name("-bad").is_err());
    }

    #[test]
    fn validate_name_ending_with_slash() {
        assert!(validate_branch_name("bad/").is_err());
    }

    #[test]
    fn validate_name_starting_with_slash() {
        assert!(validate_branch_name("/bad").is_err());
    }

    #[test]
    fn validate_name_ending_with_dot() {
        assert!(validate_branch_name("bad.").is_err());
    }

    #[test]
    fn validate_name_with_tilde() {
        assert!(validate_branch_name("a~1").is_err());
    }

    #[test]
    fn validate_name_with_caret() {
        assert!(validate_branch_name("a^1").is_err());
    }

    #[test]
    fn validate_name_with_colon() {
        assert!(validate_branch_name("a:b").is_err());
    }

    #[test]
    fn validate_name_with_backslash() {
        assert!(validate_branch_name("a\\b").is_err());
    }

    #[test]
    fn validate_name_with_control_char() {
        assert!(validate_branch_name("a\x01b").is_err());
    }

    #[test]
    fn validate_name_with_at_brace() {
        assert!(validate_branch_name("a@{b").is_err());
    }

    #[test]
    fn validate_name_just_at() {
        assert!(validate_branch_name("@").is_err());
    }

    #[test]
    fn validate_name_ending_with_lock() {
        assert!(validate_branch_name("my.lock").is_err());
    }

    // -- Integration tests with real git repos ------------------------------

    fn test_storage(dir: &std::path::Path) -> StorageConfig {
        StorageConfig::new(dir.to_path_buf())
    }

    /// Create a bare repo and add an initial commit so branches can be created.
    async fn setup_repo_with_commit(storage: &StorageConfig, repo_id: Uuid) -> PathBuf {
        // Initialize bare repo using storage service
        let path = crate::storage::service::init_bare_repo(storage, repo_id, "main")
            .await
            .expect("failed to init bare repo");

        // Create an initial commit using a temporary clone
        let tmp_clone = path.parent().unwrap().join("tmp-clone");
        tokio::fs::create_dir_all(&tmp_clone)
            .await
            .expect("create tmp clone dir");

        // Clone the bare repo
        let output = tokio::process::Command::new("git")
            .args(["clone", path.to_str().unwrap(), tmp_clone.to_str().unwrap()])
            .output()
            .await
            .expect("git clone");
        // Clone of empty repo may warn but should succeed
        assert!(
            output.status.success() || String::from_utf8_lossy(&output.stderr).contains("empty")
        );

        // Configure git user in the clone
        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config email");

        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config name");

        // Create a file, commit, and push
        tokio::fs::write(tmp_clone.join("README.md"), "# Test\n")
            .await
            .expect("write readme");

        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git add");

        tokio::process::Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git commit");

        // Push to the bare repo
        let push_output = tokio::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git push");

        if !push_output.status.success() {
            // Try pushing HEAD as main (in case default branch name differs)
            tokio::process::Command::new("git")
                .args(["push", "origin", "HEAD:refs/heads/main"])
                .current_dir(&tmp_clone)
                .output()
                .await
                .expect("git push HEAD:main");
        }

        // Clean up tmp clone
        let _ = tokio::fs::remove_dir_all(&tmp_clone).await;

        path
    }

    #[tokio::test]
    async fn list_branches_empty_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        crate::storage::service::init_bare_repo(&storage, id, "main")
            .await
            .expect("init");

        let branches = list_branches(&storage, id, "main").await.unwrap();
        assert!(branches.is_empty());
    }

    #[tokio::test]
    async fn list_branches_with_commits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let branches = list_branches(&storage, id, "main").await.unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].name, "main");
        assert!(branches[0].is_default);
        assert!(!branches[0].head_commit.is_empty());
        assert!(branches[0].head_commit.len() >= 40);
    }

    #[tokio::test]
    async fn get_branch_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let branch = get_branch(&storage, id, "main", "main").await.unwrap();
        assert!(branch.is_some());
        let info = branch.unwrap();
        assert_eq!(info.name, "main");
        assert!(info.is_default);
        assert!(!info.head_commit.is_empty());
    }

    #[tokio::test]
    async fn get_branch_nonexistent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let branch = get_branch(&storage, id, "nonexistent", "main")
            .await
            .unwrap();
        assert!(branch.is_none());
    }

    #[tokio::test]
    async fn create_and_list_branch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let new_branch = create_branch(&storage, id, "feature/test", "main", "main")
            .await
            .unwrap();
        assert_eq!(new_branch.name, "feature/test");
        assert!(!new_branch.is_default);
        assert!(!new_branch.head_commit.is_empty());

        let branches = list_branches(&storage, id, "main").await.unwrap();
        assert_eq!(branches.len(), 2);
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"feature/test"));
    }

    #[tokio::test]
    async fn create_branch_already_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let result = create_branch(&storage, id, "main", "main", "main").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_branch_invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let result = create_branch(&storage, id, "bad branch", "main", "main").await;
        assert!(result.is_err());

        let result = create_branch(&storage, id, "a..b", "main", "main").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_branch_success() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        // Create a branch to delete.
        create_branch(&storage, id, "to-delete", "main", "main")
            .await
            .unwrap();

        delete_branch(&storage, id, "to-delete", "main")
            .await
            .unwrap();

        // Verify it's gone.
        let branch = get_branch(&storage, id, "to-delete", "main").await.unwrap();
        assert!(branch.is_none());
    }

    #[tokio::test]
    async fn delete_default_branch_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let result = delete_branch(&storage, id, "main", "main").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_nonexistent_branch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let result = delete_branch(&storage, id, "no-such-branch", "main").await;
        assert!(result.is_err());
    }
}

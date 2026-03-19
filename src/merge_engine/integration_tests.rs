//! Integration tests for merge strategies.
//!
//! These tests create real Git repositories on disk with branches, commits,
//! and various file states, then exercise the three merge strategies
//! (merge commit, squash, rebase-and-merge) and conflict detection.
//!
//! All tests use `tempfile::TempDir` so artifacts are cleaned up automatically.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::storage::git::GitCommand;
    use crate::merge_engine::strategies::{
        execute_merge_commit, execute_rebase_and_merge, execute_squash_merge, MergeError,
    };
    use crate::merge_engine::service::check_conflicts;
    use crate::storage::service::StorageConfig;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Initialize a bare repo with an initial commit on "main" and return
    /// `(tempdir_handle, bare_repo_path)`.
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
            .args([
                "clone",
                repo.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Configure user
        configure_git_user(&clone_dir).await;

        // Ensure we are on the "main" branch
        let _ = tokio::process::Command::new("git")
            .args(["checkout", "-b", "main"])
            .current_dir(&clone_dir)
            .output()
            .await;

        // Create initial commit on main
        let readme = clone_dir.join("README.md");
        tokio::fs::write(&readme, "# Test Repo\n").await.unwrap();

        git_add_commit_push(&clone_dir, "initial commit", "HEAD:refs/heads/main").await;

        (tmp, repo)
    }

    /// Configure git user.name and user.email in a working directory.
    async fn configure_git_user(dir: &Path) {
        for args in [
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test User"],
        ] {
            let out = tokio::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .await
                .unwrap();
            assert!(out.status.success());
        }
    }

    /// Run `git add . && git commit -m <msg> && git push origin <refspec>`.
    async fn git_add_commit_push(dir: &Path, message: &str, refspec: &str) {
        let out = tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "git add failed");

        let out = tokio::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let out = tokio::process::Command::new("git")
            .args(["push", "origin", refspec])
            .current_dir(dir)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "git push failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Clone the bare repo, create a feature branch with one or more file
    /// changes, push it, and return the clone directory.
    async fn create_feature_branch(
        tmp: &tempfile::TempDir,
        bare_path: &Path,
        branch_name: &str,
        files: &[(&str, &str)],
    ) -> PathBuf {
        let clone_dir = tmp.path().join(format!("clone-{}", branch_name));
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare_path.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        configure_git_user(&clone_dir).await;

        // Create and checkout branch
        let out = tokio::process::Command::new("git")
            .args(["checkout", "-b", branch_name])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        // Write files
        for (name, content) in files {
            let file_path = clone_dir.join(name);
            // Create parent directories if necessary
            if let Some(parent) = file_path.parent() {
                tokio::fs::create_dir_all(parent).await.unwrap();
            }
            tokio::fs::write(&file_path, content).await.unwrap();
        }

        git_add_commit_push(&clone_dir, &format!("add files on {}", branch_name), branch_name)
            .await;

        clone_dir
    }

    /// Add multiple commits to a branch (for testing squash/rebase with
    /// multiple commits).
    async fn add_commits_to_branch(
        clone_dir: &Path,
        branch_name: &str,
        commits: &[(&str, &str, &str)], // (filename, content, message)
    ) {
        for (filename, content, message) in commits {
            let file_path = clone_dir.join(filename);
            if let Some(parent) = file_path.parent() {
                tokio::fs::create_dir_all(parent).await.unwrap();
            }
            tokio::fs::write(&file_path, content).await.unwrap();

            let out = tokio::process::Command::new("git")
                .args(["add", "."])
                .current_dir(clone_dir)
                .output()
                .await
                .unwrap();
            assert!(out.status.success());

            let out = tokio::process::Command::new("git")
                .args(["commit", "-m", message])
                .current_dir(clone_dir)
                .output()
                .await
                .unwrap();
            assert!(
                out.status.success(),
                "commit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let out = tokio::process::Command::new("git")
            .args(["push", "origin", branch_name])
            .current_dir(clone_dir)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "push failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Push a conflicting change directly to main.
    async fn push_conflicting_change_to_main(
        tmp: &tempfile::TempDir,
        bare_path: &Path,
        filename: &str,
        content: &str,
    ) {
        let clone_dir = tmp.path().join("clone-main-conflict");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare_path.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_dir).await;

        tokio::fs::write(clone_dir.join(filename), content)
            .await
            .unwrap();

        git_add_commit_push(&clone_dir, "conflicting change on main", "main").await;
    }

    /// Create a worktree from the bare repo for the target branch,
    /// configure user + remote, and fetch.
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
        let _ = tokio::process::Command::new("git")
            .args(["remote", "add", "origin", bare_path.to_str().unwrap()])
            .current_dir(&wt_path)
            .output()
            .await;

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

    /// Get a ref SHA from the bare repo.
    async fn get_ref_sha(bare_path: &Path, branch: &str) -> String {
        let git = GitCommand::new(bare_path.to_path_buf());
        let ref_name = format!("refs/heads/{}", branch);
        let output = git.run(&["rev-parse", &ref_name]).await.unwrap();
        assert!(output.success());
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Count parent commits of a commit in the bare repo.
    async fn commit_parent_count(bare_path: &Path, sha: &str) -> usize {
        let git = GitCommand::new(bare_path.to_path_buf());
        let output = git.run(&["cat-file", "-p", sha]).await.unwrap();
        assert!(output.success());
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().filter(|l| l.starts_with("parent ")).count()
    }

    /// Get the parent SHAs of a commit.
    async fn commit_parents(bare_path: &Path, sha: &str) -> Vec<String> {
        let git = GitCommand::new(bare_path.to_path_buf());
        let output = git.run(&["cat-file", "-p", sha]).await.unwrap();
        assert!(output.success());
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines()
            .filter(|l| l.starts_with("parent "))
            .map(|l| l.strip_prefix("parent ").unwrap().trim().to_string())
            .collect()
    }

    /// Get the commit message (subject line) for a SHA.
    async fn commit_message(bare_path: &Path, sha: &str) -> String {
        let git = GitCommand::new(bare_path.to_path_buf());
        let output = git
            .run(&["log", "-1", "--format=%s", sha])
            .await
            .unwrap();
        assert!(output.success());
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Count commits between two refs (exclusive..inclusive).
    async fn count_commits_between(bare_path: &Path, from: &str, to: &str) -> usize {
        let git = GitCommand::new(bare_path.to_path_buf());
        let range = format!("{}..{}", from, to);
        let output = git
            .run(&["rev-list", "--count", &range])
            .await
            .unwrap();
        assert!(output.success());
        let count_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        count_str.parse::<usize>().unwrap_or(0)
    }

    /// Check if a file exists at a given commit SHA.
    async fn file_exists_at_commit(bare_path: &Path, sha: &str, filename: &str) -> bool {
        let git = GitCommand::new(bare_path.to_path_buf());
        let spec = format!("{}:{}", sha, filename);
        let output = git.run(&["cat-file", "-t", &spec]).await.unwrap();
        output.success()
    }

    /// Read the content of a file at a given commit SHA.
    async fn file_content_at_commit(bare_path: &Path, sha: &str, filename: &str) -> String {
        let git = GitCommand::new(bare_path.to_path_buf());
        let spec = format!("{}:{}", sha, filename);
        let output = git.run(&["cat-file", "-p", &spec]).await.unwrap();
        assert!(output.success());
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Format a MergeError for readable assertion messages.
    fn format_merge_error(e: MergeError) -> String {
        match e {
            MergeError::Internal(s) => format!("Internal: {}", s),
            MergeError::Conflict(f) => format!("Conflict: {:?}", f),
        }
    }

    // =======================================================================
    // MERGE COMMIT STRATEGY TESTS
    // =======================================================================

    #[tokio::test]
    async fn merge_commit_creates_two_parent_commit() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(&tmp, &bare, "feat-mc-1", &[("feature.txt", "hello\n")])
            .await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;
        let feature_sha = get_ref_sha(&bare, "feat-mc-1").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let merge_sha = execute_merge_commit(
            &git,
            &wt_path,
            "feat-mc-1",
            "main",
            "Merge feat-mc-1 into main",
        )
        .await
        .unwrap_or_else(|e| panic!("merge_commit failed: {}", format_merge_error(e)));

        assert!(!merge_sha.is_empty());
        assert_ne!(merge_sha, main_sha_before);

        // Verify two parents
        let parents = commit_parents(&bare, &merge_sha).await;
        assert_eq!(parents.len(), 2, "merge commit should have exactly 2 parents");
        assert!(parents.contains(&main_sha_before));
        assert!(parents.contains(&feature_sha));

        // Verify the bare repo ref was updated
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_after, merge_sha);
    }

    #[tokio::test]
    async fn merge_commit_preserves_file_content() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-mc-content",
            &[("new_file.txt", "feature content\n")],
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let merge_sha = execute_merge_commit(
            &git,
            &wt_path,
            "feat-mc-content",
            "main",
            "Merge feature content",
        )
        .await
        .unwrap_or_else(|e| panic!("merge failed: {}", format_merge_error(e)));

        // After merge, both the original README and the new file should exist
        assert!(file_exists_at_commit(&bare, &merge_sha, "README.md").await);
        assert!(file_exists_at_commit(&bare, &merge_sha, "new_file.txt").await);

        let content = file_content_at_commit(&bare, &merge_sha, "new_file.txt").await;
        assert_eq!(content, "feature content\n");
    }

    #[tokio::test]
    async fn merge_commit_with_custom_message() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(&tmp, &bare, "feat-mc-msg", &[("msg.txt", "data\n")]).await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let merge_sha = execute_merge_commit(
            &git,
            &wt_path,
            "feat-mc-msg",
            "main",
            "My custom merge message",
        )
        .await
        .unwrap();

        let msg = commit_message(&bare, &merge_sha).await;
        assert_eq!(msg, "My custom merge message");
    }

    #[tokio::test]
    async fn merge_commit_with_multiple_files() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-mc-multi",
            &[
                ("file_a.txt", "content a\n"),
                ("file_b.txt", "content b\n"),
                ("dir/file_c.txt", "content c\n"),
            ],
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let merge_sha = execute_merge_commit(
            &git,
            &wt_path,
            "feat-mc-multi",
            "main",
            "Merge multiple files",
        )
        .await
        .unwrap_or_else(|e| panic!("merge failed: {}", format_merge_error(e)));

        // All files from the feature branch should be present
        assert!(file_exists_at_commit(&bare, &merge_sha, "file_a.txt").await);
        assert!(file_exists_at_commit(&bare, &merge_sha, "file_b.txt").await);
        assert!(file_exists_at_commit(&bare, &merge_sha, "dir/file_c.txt").await);
        // Original file should still be there
        assert!(file_exists_at_commit(&bare, &merge_sha, "README.md").await);
    }

    #[tokio::test]
    async fn merge_commit_does_not_modify_source_branch() {
        let (tmp, bare) = init_test_repo().await;
        let _ =
            create_feature_branch(&tmp, &bare, "feat-mc-src", &[("src.txt", "source\n")]).await;

        let git = GitCommand::new(bare.clone());
        let feature_sha_before = get_ref_sha(&bare, "feat-mc-src").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let _ = execute_merge_commit(
            &git,
            &wt_path,
            "feat-mc-src",
            "main",
            "Merge source",
        )
        .await
        .unwrap();

        // Feature branch ref should be unchanged
        let feature_sha_after = get_ref_sha(&bare, "feat-mc-src").await;
        assert_eq!(feature_sha_before, feature_sha_after);
    }

    // =======================================================================
    // SQUASH MERGE STRATEGY TESTS
    // =======================================================================

    #[tokio::test]
    async fn squash_merge_creates_single_parent_commit() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(&tmp, &bare, "feat-sq-1", &[("squash.txt", "squashed\n")])
            .await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let squash_sha = execute_squash_merge(
            &git,
            &wt_path,
            "feat-sq-1",
            "main",
            "Squash feat-sq-1 into main",
        )
        .await
        .unwrap_or_else(|e| panic!("squash_merge failed: {}", format_merge_error(e)));

        assert!(!squash_sha.is_empty());
        assert_ne!(squash_sha, main_sha_before);

        // Squash commit should have exactly ONE parent
        let parents = commit_parents(&bare, &squash_sha).await;
        assert_eq!(parents.len(), 1, "squash commit should have exactly 1 parent");
        assert_eq!(parents[0], main_sha_before);

        // Verify the bare repo ref was updated
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_after, squash_sha);
    }

    #[tokio::test]
    async fn squash_merge_condenses_multiple_commits() {
        let (tmp, bare) = init_test_repo().await;

        // Create a feature branch with multiple commits
        let clone_dir = create_feature_branch(
            &tmp,
            &bare,
            "feat-sq-multi",
            &[("file1.txt", "first\n")],
        )
        .await;

        // Add more commits
        add_commits_to_branch(
            &clone_dir,
            "feat-sq-multi",
            &[
                ("file2.txt", "second\n", "add second file"),
                ("file3.txt", "third\n", "add third file"),
            ],
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let squash_sha = execute_squash_merge(
            &git,
            &wt_path,
            "feat-sq-multi",
            "main",
            "Squash all feature commits",
        )
        .await
        .unwrap_or_else(|e| panic!("squash failed: {}", format_merge_error(e)));

        // Even though the feature had 3 commits, only 1 new commit should appear
        // on main after squash.
        let count = count_commits_between(&bare, &main_sha_before, &squash_sha).await;
        assert_eq!(
            count, 1,
            "squash merge should create exactly 1 commit, got {}",
            count
        );

        // All files from all feature commits should be present
        assert!(file_exists_at_commit(&bare, &squash_sha, "file1.txt").await);
        assert!(file_exists_at_commit(&bare, &squash_sha, "file2.txt").await);
        assert!(file_exists_at_commit(&bare, &squash_sha, "file3.txt").await);
    }

    #[tokio::test]
    async fn squash_merge_with_custom_message() {
        let (tmp, bare) = init_test_repo().await;
        let _ =
            create_feature_branch(&tmp, &bare, "feat-sq-msg", &[("sqm.txt", "content\n")]).await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let squash_sha = execute_squash_merge(
            &git,
            &wt_path,
            "feat-sq-msg",
            "main",
            "Custom squash message",
        )
        .await
        .unwrap();

        let msg = commit_message(&bare, &squash_sha).await;
        assert_eq!(msg, "Custom squash message");
    }

    #[tokio::test]
    async fn squash_merge_preserves_all_changes() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-sq-changes",
            &[
                ("alpha.txt", "alpha content\n"),
                ("nested/beta.txt", "beta content\n"),
            ],
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let squash_sha = execute_squash_merge(
            &git,
            &wt_path,
            "feat-sq-changes",
            "main",
            "Squash changes",
        )
        .await
        .unwrap_or_else(|e| panic!("squash failed: {}", format_merge_error(e)));

        assert!(file_exists_at_commit(&bare, &squash_sha, "alpha.txt").await);
        assert!(file_exists_at_commit(&bare, &squash_sha, "nested/beta.txt").await);

        let alpha = file_content_at_commit(&bare, &squash_sha, "alpha.txt").await;
        assert_eq!(alpha, "alpha content\n");

        let beta = file_content_at_commit(&bare, &squash_sha, "nested/beta.txt").await;
        assert_eq!(beta, "beta content\n");
    }

    #[tokio::test]
    async fn squash_merge_does_not_modify_source_branch() {
        let (tmp, bare) = init_test_repo().await;
        let _ =
            create_feature_branch(&tmp, &bare, "feat-sq-src", &[("src.txt", "source\n")]).await;

        let git = GitCommand::new(bare.clone());
        let feature_sha_before = get_ref_sha(&bare, "feat-sq-src").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let _ = execute_squash_merge(
            &git,
            &wt_path,
            "feat-sq-src",
            "main",
            "Squash",
        )
        .await
        .unwrap();

        let feature_sha_after = get_ref_sha(&bare, "feat-sq-src").await;
        assert_eq!(feature_sha_before, feature_sha_after);
    }

    // =======================================================================
    // REBASE AND MERGE STRATEGY TESTS
    // =======================================================================

    #[tokio::test]
    async fn rebase_and_merge_creates_linear_history() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-rb-1",
            &[("rebase.txt", "rebased content\n")],
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let new_sha = execute_rebase_and_merge(
            &git,
            &wt_path,
            "feat-rb-1",
            "main",
            "Rebase and merge",
        )
        .await
        .unwrap_or_else(|e| panic!("rebase_and_merge failed: {}", format_merge_error(e)));

        assert!(!new_sha.is_empty());
        assert_ne!(new_sha, main_sha_before);

        // Result should be a single-parent commit (linear history)
        let parents = commit_parents(&bare, &new_sha).await;
        assert_eq!(parents.len(), 1, "rebased commit should have exactly 1 parent");

        // The parent should be the old main HEAD
        assert_eq!(parents[0], main_sha_before);

        // Verify the bare repo ref was updated
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_after, new_sha);
    }

    #[tokio::test]
    async fn rebase_and_merge_replays_multiple_commits() {
        let (tmp, bare) = init_test_repo().await;

        // Create feature branch with multiple commits
        let clone_dir = create_feature_branch(
            &tmp,
            &bare,
            "feat-rb-multi",
            &[("rb_file1.txt", "first\n")],
        )
        .await;

        add_commits_to_branch(
            &clone_dir,
            "feat-rb-multi",
            &[
                ("rb_file2.txt", "second\n", "add second file"),
                ("rb_file3.txt", "third\n", "add third file"),
            ],
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let new_sha = execute_rebase_and_merge(
            &git,
            &wt_path,
            "feat-rb-multi",
            "main",
            "Rebase and merge",
        )
        .await
        .unwrap_or_else(|e| panic!("rebase failed: {}", format_merge_error(e)));

        // Rebase should replay all 3 commits (initial branch + 2 additional)
        let count = count_commits_between(&bare, &main_sha_before, &new_sha).await;
        assert_eq!(
            count, 3,
            "rebase should replay all 3 feature commits, got {}",
            count
        );

        // All files should be present
        assert!(file_exists_at_commit(&bare, &new_sha, "rb_file1.txt").await);
        assert!(file_exists_at_commit(&bare, &new_sha, "rb_file2.txt").await);
        assert!(file_exists_at_commit(&bare, &new_sha, "rb_file3.txt").await);

        // Each commit in the chain should have exactly 1 parent (linear)
        let git2 = GitCommand::new(bare.clone());
        let range = format!("{}..{}", main_sha_before, new_sha);
        let output = git2
            .run(&["rev-list", &range])
            .await
            .unwrap();
        assert!(output.success());
        let shas: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        for sha in &shas {
            let pc = commit_parent_count(&bare, sha).await;
            assert_eq!(pc, 1, "each rebased commit should have 1 parent, sha={}", sha);
        }
    }

    #[tokio::test]
    async fn rebase_and_merge_preserves_file_content() {
        let (tmp, bare) = init_test_repo().await;
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-rb-content",
            &[("rebase_data.txt", "important data\n")],
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let new_sha = execute_rebase_and_merge(
            &git,
            &wt_path,
            "feat-rb-content",
            "main",
            "Rebase content",
        )
        .await
        .unwrap_or_else(|e| panic!("rebase failed: {}", format_merge_error(e)));

        assert!(file_exists_at_commit(&bare, &new_sha, "rebase_data.txt").await);
        assert!(file_exists_at_commit(&bare, &new_sha, "README.md").await);

        let content = file_content_at_commit(&bare, &new_sha, "rebase_data.txt").await;
        assert_eq!(content, "important data\n");
    }

    #[tokio::test]
    async fn rebase_and_merge_does_not_modify_source_branch() {
        let (tmp, bare) = init_test_repo().await;
        let _ =
            create_feature_branch(&tmp, &bare, "feat-rb-src", &[("src.txt", "source\n")]).await;

        let git = GitCommand::new(bare.clone());
        let feature_sha_before = get_ref_sha(&bare, "feat-rb-src").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let _ = execute_rebase_and_merge(
            &git,
            &wt_path,
            "feat-rb-src",
            "main",
            "Rebase",
        )
        .await
        .unwrap();

        let feature_sha_after = get_ref_sha(&bare, "feat-rb-src").await;
        assert_eq!(feature_sha_before, feature_sha_after);
    }

    // =======================================================================
    // CONFLICT DETECTION TESTS -- STRATEGY-LEVEL
    // =======================================================================

    #[tokio::test]
    async fn merge_commit_detects_conflict_on_same_file() {
        let (tmp, bare) = init_test_repo().await;

        // Create a feature branch that modifies shared.txt
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "conflict-mc",
            &[("shared.txt", "content from feature branch\n")],
        )
        .await;

        // Push a conflicting change to main
        push_conflicting_change_to_main(
            &tmp,
            &bare,
            "shared.txt",
            "content from main branch\n",
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_merge_commit(
            &git,
            &wt_path,
            "conflict-mc",
            "main",
            "This should conflict",
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

        // Verify main ref was NOT modified (conflict should not update ref)
        let main_sha = get_ref_sha(&bare, "main").await;
        // main_sha should still be the one after the conflicting push, not a merge
        let parents = commit_parent_count(&bare, &main_sha).await;
        assert_eq!(
            parents, 1,
            "main should not have a merge commit after failed merge"
        );
    }

    #[tokio::test]
    async fn squash_merge_detects_conflict_on_same_file() {
        let (tmp, bare) = init_test_repo().await;

        let _ = create_feature_branch(
            &tmp,
            &bare,
            "conflict-sq",
            &[("shared.txt", "squash feature content\n")],
        )
        .await;

        push_conflicting_change_to_main(
            &tmp,
            &bare,
            "shared.txt",
            "main has different content\n",
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_squash_merge(
            &git,
            &wt_path,
            "conflict-sq",
            "main",
            "This should conflict",
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
                panic!("expected conflict, but squash succeeded with sha: {}", sha);
            }
        }

        // Main ref should be unchanged
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_before, main_sha_after);
    }

    #[tokio::test]
    async fn rebase_and_merge_detects_conflict_on_same_file() {
        let (tmp, bare) = init_test_repo().await;

        let _ = create_feature_branch(
            &tmp,
            &bare,
            "conflict-rb",
            &[("shared.txt", "rebase feature content\n")],
        )
        .await;

        push_conflicting_change_to_main(
            &tmp,
            &bare,
            "shared.txt",
            "main rebase conflict content\n",
        )
        .await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_rebase_and_merge(
            &git,
            &wt_path,
            "conflict-rb",
            "main",
            "This should conflict",
        )
        .await;

        match result {
            Err(MergeError::Conflict(_files)) => {
                // Success -- conflict was detected
            }
            Err(MergeError::Internal(msg)) => {
                panic!("expected Conflict error, got Internal: {}", msg);
            }
            Ok(sha) => {
                panic!("expected conflict, but rebase succeeded with sha: {}", sha);
            }
        }

        // Main ref should be unchanged
        let main_sha_after = get_ref_sha(&bare, "main").await;
        assert_eq!(main_sha_before, main_sha_after);
    }

    #[tokio::test]
    async fn conflict_with_multiple_files() {
        let (tmp, bare) = init_test_repo().await;

        // Feature branch modifies two files
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "conflict-multi",
            &[
                ("file_x.txt", "feature version of x\n"),
                ("file_y.txt", "feature version of y\n"),
            ],
        )
        .await;

        // Push conflicting changes to main for both files
        let clone_main = tmp.path().join("clone-main-multi-conflict");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                clone_main.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_main).await;

        tokio::fs::write(clone_main.join("file_x.txt"), "main version of x\n")
            .await
            .unwrap();
        tokio::fs::write(clone_main.join("file_y.txt"), "main version of y\n")
            .await
            .unwrap();

        git_add_commit_push(&clone_main, "add conflicting files on main", "main").await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_merge_commit(
            &git,
            &wt_path,
            "conflict-multi",
            "main",
            "This should conflict on multiple files",
        )
        .await;

        match result {
            Err(MergeError::Conflict(files)) => {
                assert!(
                    files.len() >= 2,
                    "expected at least 2 conflicting files, got: {:?}",
                    files
                );
                assert!(files.contains(&"file_x.txt".to_string()));
                assert!(files.contains(&"file_y.txt".to_string()));
            }
            Err(MergeError::Internal(msg)) => {
                panic!("expected Conflict, got Internal: {}", msg);
            }
            Ok(sha) => {
                panic!("expected conflict, got success: {}", sha);
            }
        }
    }

    #[tokio::test]
    async fn no_conflict_when_different_files_modified() {
        let (tmp, bare) = init_test_repo().await;

        // Feature branch adds file_a.txt
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-noconflict",
            &[("file_a.txt", "feature file a\n")],
        )
        .await;

        // Main adds file_b.txt (no overlap)
        let clone_main = tmp.path().join("clone-main-noconflict");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                clone_main.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_main).await;

        tokio::fs::write(clone_main.join("file_b.txt"), "main file b\n")
            .await
            .unwrap();

        git_add_commit_push(&clone_main, "add file_b on main", "main").await;

        // Merge should succeed (no conflict)
        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let result = execute_merge_commit(
            &git,
            &wt_path,
            "feat-noconflict",
            "main",
            "Merge non-conflicting changes",
        )
        .await;

        assert!(
            result.is_ok(),
            "expected successful merge, got: {}",
            format_merge_error(result.unwrap_err())
        );

        let merge_sha = result.unwrap();

        // Both files should exist
        assert!(file_exists_at_commit(&bare, &merge_sha, "file_a.txt").await);
        assert!(file_exists_at_commit(&bare, &merge_sha, "file_b.txt").await);
    }

    // =======================================================================
    // CONFLICT CHECK SERVICE TESTS
    // =======================================================================

    #[tokio::test]
    async fn check_conflicts_no_conflicts() {
        let (tmp, bare) = init_test_repo().await;

        // Feature branch adds a new file (no overlap with main)
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-cc-ok",
            &[("unique_file.txt", "unique\n")],
        )
        .await;

        // Set up a StorageConfig that maps the repo_id to the bare repo path.
        // We need to create the directory structure that repo_path() expects.
        let repo_id = uuid::Uuid::new_v4();
        let id_str = repo_id.to_string();
        let prefix = &id_str[..2];
        let storage_root = tmp.path().join("storage");
        let fanout_dir = storage_root.join(prefix);
        tokio::fs::create_dir_all(&fanout_dir).await.unwrap();

        // Symlink or rename bare repo to match expected path
        let expected_path = fanout_dir.join(format!("{}.git", id_str));
        tokio::fs::rename(&bare, &expected_path).await.unwrap();

        let config = StorageConfig::new(storage_root);

        let result = check_conflicts(&config, repo_id, "feat-cc-ok", "main").await;

        assert!(result.is_ok(), "check_conflicts failed: {:?}", result.err());
        let check = result.unwrap();
        assert!(!check.has_conflicts, "expected no conflicts");
        assert!(check.conflicting_files.is_empty());
    }

    #[tokio::test]
    async fn check_conflicts_detects_conflicts() {
        let (tmp, bare) = init_test_repo().await;

        // Feature branch modifies shared.txt
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-cc-conflict",
            &[("shared.txt", "feature content\n")],
        )
        .await;

        // Push conflicting change to main
        push_conflicting_change_to_main(
            &tmp,
            &bare,
            "shared.txt",
            "main content\n",
        )
        .await;

        // Set up StorageConfig
        let repo_id = uuid::Uuid::new_v4();
        let id_str = repo_id.to_string();
        let prefix = &id_str[..2];
        let storage_root = tmp.path().join("storage-conflict");
        let fanout_dir = storage_root.join(prefix);
        tokio::fs::create_dir_all(&fanout_dir).await.unwrap();

        let expected_path = fanout_dir.join(format!("{}.git", id_str));
        tokio::fs::rename(&bare, &expected_path).await.unwrap();

        let config = StorageConfig::new(storage_root);

        let result = check_conflicts(&config, repo_id, "feat-cc-conflict", "main").await;

        assert!(result.is_ok(), "check_conflicts failed: {:?}", result.err());
        let check = result.unwrap();
        assert!(check.has_conflicts, "expected conflicts to be detected");
    }

    // =======================================================================
    // CROSS-STRATEGY COMPARISON TESTS
    // =======================================================================

    #[tokio::test]
    async fn all_strategies_include_feature_file_after_merge() {
        // Verify that regardless of strategy, the feature file ends up on main.

        for (strategy_name, idx) in [("merge_commit", 0), ("squash", 1), ("rebase", 2)] {
            let (tmp, bare) = init_test_repo().await;
            let branch_name = format!("feat-all-{}", idx);
            let filename = format!("strategy_{}.txt", idx);
            let _ = create_feature_branch(
                &tmp,
                &bare,
                &branch_name,
                &[(&filename, "strategy content\n")],
            )
            .await;

            let git = GitCommand::new(bare.clone());
            let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

            let result_sha = match idx {
                0 => execute_merge_commit(
                    &git,
                    &wt_path,
                    &branch_name,
                    "main",
                    "merge commit strategy",
                )
                .await,
                1 => execute_squash_merge(
                    &git,
                    &wt_path,
                    &branch_name,
                    "main",
                    "squash strategy",
                )
                .await,
                2 => execute_rebase_and_merge(
                    &git,
                    &wt_path,
                    &branch_name,
                    "main",
                    "rebase strategy",
                )
                .await,
                _ => unreachable!(),
            };

            let sha = result_sha.unwrap_or_else(|e| {
                panic!(
                    "{} strategy failed: {}",
                    strategy_name,
                    format_merge_error(e)
                )
            });

            assert!(
                file_exists_at_commit(&bare, &sha, &filename).await,
                "{} strategy: expected {} to exist at commit {}",
                strategy_name,
                filename,
                sha
            );

            let content = file_content_at_commit(&bare, &sha, &filename).await;
            assert_eq!(
                content, "strategy content\n",
                "{} strategy: file content mismatch",
                strategy_name
            );
        }
    }

    #[tokio::test]
    async fn merge_commit_has_two_parents_while_others_have_one() {
        // Verify the fundamental structural difference between strategies.

        let strategies: Vec<(&str, usize)> =
            vec![("merge_commit", 2), ("squash", 1), ("rebase", 1)];

        for (strategy_name, expected_parents) in strategies {
            let (tmp, bare) = init_test_repo().await;
            let branch_name = format!("feat-parents-{}", strategy_name);
            let _ = create_feature_branch(
                &tmp,
                &bare,
                &branch_name,
                &[(&format!("{}.txt", strategy_name), "data\n")],
            )
            .await;

            let git = GitCommand::new(bare.clone());
            let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

            let sha = match strategy_name {
                "merge_commit" => execute_merge_commit(
                    &git,
                    &wt_path,
                    &branch_name,
                    "main",
                    "merge",
                )
                .await,
                "squash" => execute_squash_merge(
                    &git,
                    &wt_path,
                    &branch_name,
                    "main",
                    "squash",
                )
                .await,
                "rebase" => execute_rebase_and_merge(
                    &git,
                    &wt_path,
                    &branch_name,
                    "main",
                    "rebase",
                )
                .await,
                _ => unreachable!(),
            }
            .unwrap_or_else(|e| {
                panic!("{} failed: {}", strategy_name, format_merge_error(e))
            });

            let parents = commit_parent_count(&bare, &sha).await;
            assert_eq!(
                parents, expected_parents,
                "{} strategy: expected {} parents, got {}",
                strategy_name, expected_parents, parents
            );
        }
    }

    // =======================================================================
    // EDGE CASE TESTS
    // =======================================================================

    #[tokio::test]
    async fn merge_commit_with_file_modification_not_addition() {
        // Test merging a branch that modifies an existing file rather than
        // adding a new one.
        let (tmp, bare) = init_test_repo().await;

        // Feature branch modifies README.md
        let clone_dir = tmp.path().join("clone-modify");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                clone_dir.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_dir).await;

        let out = tokio::process::Command::new("git")
            .args(["checkout", "-b", "feat-modify"])
            .current_dir(&clone_dir)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        tokio::fs::write(clone_dir.join("README.md"), "# Modified Repo\n\nNew content.\n")
            .await
            .unwrap();

        git_add_commit_push(&clone_dir, "modify README", "feat-modify").await;

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let merge_sha = execute_merge_commit(
            &git,
            &wt_path,
            "feat-modify",
            "main",
            "Merge readme modification",
        )
        .await
        .unwrap_or_else(|e| panic!("merge failed: {}", format_merge_error(e)));

        let content = file_content_at_commit(&bare, &merge_sha, "README.md").await;
        assert_eq!(content, "# Modified Repo\n\nNew content.\n");
    }

    #[tokio::test]
    async fn squash_merge_with_file_deletion() {
        // Test squash merging a branch that deletes a file.
        let (tmp, bare) = init_test_repo().await;

        // First add a file to main that the feature branch will delete
        let clone_add = tmp.path().join("clone-add-file");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                clone_add.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_add).await;

        tokio::fs::write(clone_add.join("to_delete.txt"), "will be deleted\n")
            .await
            .unwrap();

        git_add_commit_push(&clone_add, "add file to delete", "main").await;

        // Feature branch deletes to_delete.txt
        let clone_del = tmp.path().join("clone-delete");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                clone_del.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_del).await;

        let out = tokio::process::Command::new("git")
            .args(["checkout", "-b", "feat-delete"])
            .current_dir(&clone_del)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["rm", "to_delete.txt"])
            .current_dir(&clone_del)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["commit", "-m", "delete file"])
            .current_dir(&clone_del)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let out = tokio::process::Command::new("git")
            .args(["push", "origin", "feat-delete"])
            .current_dir(&clone_del)
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        let git = GitCommand::new(bare.clone());
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let squash_sha = execute_squash_merge(
            &git,
            &wt_path,
            "feat-delete",
            "main",
            "Squash delete file",
        )
        .await
        .unwrap_or_else(|e| panic!("squash failed: {}", format_merge_error(e)));

        // The file should no longer exist
        assert!(!file_exists_at_commit(&bare, &squash_sha, "to_delete.txt").await);
        // README should still exist
        assert!(file_exists_at_commit(&bare, &squash_sha, "README.md").await);
    }

    #[tokio::test]
    async fn rebase_and_merge_onto_diverged_main() {
        // Test rebase when main has advanced since the branch point.
        let (tmp, bare) = init_test_repo().await;

        // Create feature branch
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-rb-diverge",
            &[("feature_only.txt", "feature\n")],
        )
        .await;

        // Advance main with a non-conflicting change
        let clone_main = tmp.path().join("clone-main-advance");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                clone_main.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_main).await;

        tokio::fs::write(clone_main.join("main_only.txt"), "main advance\n")
            .await
            .unwrap();

        git_add_commit_push(&clone_main, "advance main", "main").await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;
        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let new_sha = execute_rebase_and_merge(
            &git,
            &wt_path,
            "feat-rb-diverge",
            "main",
            "Rebase onto diverged main",
        )
        .await
        .unwrap_or_else(|e| panic!("rebase failed: {}", format_merge_error(e)));

        // Both files should be present
        assert!(file_exists_at_commit(&bare, &new_sha, "feature_only.txt").await);
        assert!(file_exists_at_commit(&bare, &new_sha, "main_only.txt").await);

        // The rebased commit's parent chain should include the main advance
        // (since we rebased onto the new main tip).
        let parents = commit_parents(&bare, &new_sha).await;
        assert_eq!(parents.len(), 1);
        // The parent should be the advanced main commit
        assert_eq!(parents[0], main_sha_before);
    }

    #[tokio::test]
    async fn merge_commit_onto_diverged_main() {
        // Test merge commit when main has advanced since the branch point.
        let (tmp, bare) = init_test_repo().await;

        // Create feature branch
        let _ = create_feature_branch(
            &tmp,
            &bare,
            "feat-mc-diverge",
            &[("feat_file.txt", "feature\n")],
        )
        .await;

        // Advance main
        let clone_main = tmp.path().join("clone-main-mc-advance");
        let out = tokio::process::Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                clone_main.to_str().unwrap(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success());

        configure_git_user(&clone_main).await;

        tokio::fs::write(clone_main.join("main_new.txt"), "main data\n")
            .await
            .unwrap();

        git_add_commit_push(&clone_main, "advance main for mc diverge", "main").await;

        let git = GitCommand::new(bare.clone());
        let main_sha_before = get_ref_sha(&bare, "main").await;
        let feature_sha = get_ref_sha(&bare, "feat-mc-diverge").await;

        let (_wt_tmp, wt_path) = setup_worktree(&bare, "main").await;

        let merge_sha = execute_merge_commit(
            &git,
            &wt_path,
            "feat-mc-diverge",
            "main",
            "Merge diverged branches",
        )
        .await
        .unwrap_or_else(|e| panic!("merge failed: {}", format_merge_error(e)));

        // Merge commit should have 2 parents
        let parents = commit_parents(&bare, &merge_sha).await;
        assert_eq!(parents.len(), 2);
        assert!(parents.contains(&main_sha_before));
        assert!(parents.contains(&feature_sha));

        // Both files should exist
        assert!(file_exists_at_commit(&bare, &merge_sha, "feat_file.txt").await);
        assert!(file_exists_at_commit(&bare, &merge_sha, "main_new.txt").await);
    }
}

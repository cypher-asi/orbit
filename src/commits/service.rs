use base64::Engine;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::errors::ApiError;
use crate::storage::git::GitCommand;
use crate::storage::service::{repo_path, StorageConfig};

use super::models::{CommitInfo, DiffEntry, DiffStatus, FileContent, TreeEntry, TreeEntryType};

// ---------------------------------------------------------------------------
// Record separator used to delimit commits in git log output
// ---------------------------------------------------------------------------
const RECORD_SEP: &str = "\x1e";

/// Build the git log format string.
///
/// Fields are separated by newlines, records by the ASCII record separator.
/// Format: sha, author_name, author_email, committer_name, committer_email,
///         subject (%s), author date ISO, parent shas (space-separated).
fn log_format() -> String {
    format!("{}%H%n%an%n%ae%n%cn%n%ce%n%s%n%aI%n%P", RECORD_SEP)
}

/// Parse a single commit record (the 8 lines produced by the format string)
/// into a `CommitInfo`.
fn parse_commit_record(record: &str) -> Option<CommitInfo> {
    let lines: Vec<&str> = record.lines().collect();
    if lines.len() < 7 {
        return None;
    }

    let sha = lines[0].to_string();
    if sha.is_empty() {
        return None;
    }

    let author_name = lines[1].to_string();
    let author_email = lines[2].to_string();
    let committer_name = lines[3].to_string();
    let committer_email = lines[4].to_string();
    let message = lines[5].to_string();
    let timestamp_str = lines[6];

    let timestamp = DateTime::parse_from_rfc3339(timestamp_str)
        .ok()?
        .with_timezone(&Utc);

    // Parent SHAs are on line 7 (may be empty for root commit)
    let parent_shas = if lines.len() > 7 && !lines[7].is_empty() {
        lines[7].split(' ').map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    };

    Some(CommitInfo {
        sha,
        author_name,
        author_email,
        committer_name,
        committer_email,
        message,
        timestamp,
        parent_shas,
    })
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// List commits for a given ref (branch, tag, or SHA).
///
/// Runs `git log` with a structured format string and parses the output.
/// Supports pagination via `limit` and `offset`.
pub async fn list_commits(
    storage: &StorageConfig,
    repo_id: Uuid,
    ref_name: &str,
    limit: u32,
    offset: u32,
) -> Result<Vec<CommitInfo>, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    let format_arg = format!("--format={}", log_format());
    let limit_arg = format!("-n {}", limit);
    let skip_arg = format!("--skip={}", offset);

    let output = git
        .run(&["log", &format_arg, &limit_arg, &skip_arg, ref_name])
        .await?;

    if !output.success() {
        // Could be an empty repo or invalid ref
        let stderr = &output.stderr;
        if stderr.contains("unknown revision")
            || stderr.contains("bad default revision")
            || stderr.contains("does not have any commits")
        {
            return Ok(Vec::new());
        }
        tracing::warn!(
            repo_id = %repo_id,
            ref_name = %ref_name,
            stderr = %stderr,
            "git log failed"
        );
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    for record in stdout.split(RECORD_SEP) {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        if let Some(commit) = parse_commit_record(record) {
            commits.push(commit);
        }
    }

    Ok(commits)
}

/// Get details for a single commit by SHA.
///
/// Runs `git show --no-patch` with a structured format string and parses
/// the output. Returns `None` if the SHA does not exist.
pub async fn get_commit(
    storage: &StorageConfig,
    repo_id: Uuid,
    sha: &str,
) -> Result<Option<CommitInfo>, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    let format_arg = format!("--format={}", log_format());

    let output = git.run(&["show", "--no-patch", &format_arg, sha]).await?;

    if !output.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // There may be a leading record separator; split and find the first
    // non-empty record.
    for record in stdout.split(RECORD_SEP) {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        if let Some(commit) = parse_commit_record(record) {
            return Ok(Some(commit));
        }
    }

    Ok(None)
}

/// Get the list of changed files for a commit.
///
/// Runs `git diff-tree -r --numstat --no-commit-id {sha}` and parses the
/// tab-separated output: `additions\tdeletions\tpath`.
pub async fn get_commit_diff(
    storage: &StorageConfig,
    repo_id: Uuid,
    sha: &str,
) -> Result<Vec<DiffEntry>, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    // First, get numstat for additions/deletions.
    // The --root flag is needed so that the initial commit (no parent)
    // still shows its added files.
    let numstat_output = git
        .run(&[
            "diff-tree",
            "-r",
            "--root",
            "--numstat",
            "--no-commit-id",
            sha,
        ])
        .await?;

    if !numstat_output.success() {
        let stderr = &numstat_output.stderr;
        if stderr.contains("unknown revision") || stderr.contains("bad object") {
            return Err(ApiError::NotFound(format!("commit '{}' not found", sha)));
        }
        tracing::warn!(
            repo_id = %repo_id,
            sha = %sha,
            stderr = %stderr,
            "git diff-tree --numstat failed"
        );
        return Ok(Vec::new());
    }

    // Also get the status letters (A, M, D, R) for each file.
    // The --root flag is needed so that the initial commit (no parent)
    // still shows its added files.
    let status_output = git
        .run(&[
            "diff-tree",
            "-r",
            "--root",
            "--no-commit-id",
            "--name-status",
            sha,
        ])
        .await?;

    let numstat_stdout = String::from_utf8_lossy(&numstat_output.stdout);
    let status_stdout = String::from_utf8_lossy(&status_output.stdout);

    // Build a map of path -> status from --name-status output
    let mut status_map = std::collections::HashMap::new();
    for line in status_stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() == 2 {
            let status_char = parts[0].trim();
            let file_path = parts[1].trim();
            status_map.insert(file_path.to_string(), status_char.to_string());
        }
    }

    let mut entries = Vec::new();

    for line in numstat_stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: additions\tdeletions\tpath
        // Binary files show as: -\t-\tpath
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }

        let additions = parts[0].parse::<u32>().unwrap_or(0);
        let deletions = parts[1].parse::<u32>().unwrap_or(0);
        let file_path = parts[2].to_string();

        // Look up status from name-status output
        let status = match status_map.get(&file_path).map(|s| s.as_str()) {
            Some(s) if s.starts_with('A') => DiffStatus::Added,
            Some(s) if s.starts_with('D') => DiffStatus::Deleted,
            Some(s) if s.starts_with('R') => DiffStatus::Renamed,
            Some(s) if s.starts_with('M') => DiffStatus::Modified,
            _ => DiffStatus::Modified, // default
        };

        entries.push(DiffEntry {
            path: file_path,
            status,
            additions,
            deletions,
        });
    }

    Ok(entries)
}

/// Maximum file size (in bytes) that will be returned by `get_file_content`.
/// Files larger than this threshold are rejected with a 422 error.
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

/// Browse the repository tree at a given ref and optional sub-path.
///
/// Runs `git ls-tree -l {ref} {path}` and parses the output. Each line has
/// the format: `mode type sha size\tpath`. The `-l` flag includes the size
/// for blobs (shown as `-` for trees).
pub async fn list_tree(
    storage: &StorageConfig,
    repo_id: Uuid,
    ref_name: &str,
    path: Option<&str>,
) -> Result<Vec<TreeEntry>, ApiError> {
    let repo = repo_path(storage, repo_id);
    let git = GitCommand::new(repo);

    // Build the ref:path spec for ls-tree.
    // When a sub-path is provided we append a trailing `/` so that git
    // lists the *contents* of the directory rather than the directory
    // entry itself.
    let tree_ish = match path {
        Some(p) if !p.is_empty() => {
            let p = p.trim_end_matches('/');
            format!("{}:{}/", ref_name, p)
        }
        _ => ref_name.to_string(),
    };

    let output = git.run(&["ls-tree", "-l", &tree_ish]).await?;

    if !output.success() {
        let stderr = &output.stderr;
        if stderr.contains("Not a valid object name")
            || stderr.contains("not a tree object")
            || stderr.contains("fatal: not a tree object")
            || stderr.contains("does not exist")
            || stderr.contains("unknown revision")
            || stderr.contains("bad default revision")
        {
            return Err(ApiError::NotFound(format!(
                "ref '{}' or path not found",
                ref_name
            )));
        }
        tracing::warn!(
            repo_id = %repo_id,
            ref_name = %ref_name,
            stderr = %stderr,
            "git ls-tree failed"
        );
        return Err(ApiError::Internal("failed to list tree".to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let prefix = match path {
        Some(p) if !p.is_empty() => {
            let p = p.trim_end_matches('/');
            format!("{}/", p)
        }
        _ => String::new(),
    };

    let mut entries = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: "<mode> <type> <sha> <size>\t<name>"
        // The size field is padded with spaces; for trees it is `-`.
        let Some((meta, name)) = line.split_once('\t') else {
            continue;
        };

        let parts: Vec<&str> = meta.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }

        let obj_type = parts[1];
        let sha = parts[2].to_string();
        let size_str = parts[3];
        let name = name.to_string();

        let entry_type = match obj_type {
            "blob" => TreeEntryType::Blob,
            "tree" => TreeEntryType::Tree,
            _ => continue, // skip commits (submodules) and other types
        };

        let size = match entry_type {
            TreeEntryType::Blob => size_str.parse::<u64>().ok(),
            TreeEntryType::Tree => None,
        };

        let full_path = format!("{}{}", prefix, name);

        entries.push(TreeEntry {
            name,
            path: full_path,
            entry_type,
            sha,
            size,
        });
    }

    Ok(entries)
}

/// Retrieve file content at a given ref and path.
///
/// Runs `git show {ref}:{path}` and returns the raw content. Binary files
/// are returned as base64-encoded strings with `is_binary` set to `true`.
/// Files larger than 10 MB are rejected.
pub async fn get_file_content(
    storage: &StorageConfig,
    repo_id: Uuid,
    ref_name: &str,
    path: &str,
) -> Result<FileContent, ApiError> {
    let repo = repo_path(storage, repo_id);
    let git = GitCommand::new(repo);

    let object_spec = format!("{}:{}", ref_name, path);

    // First, check the object size via cat-file to enforce limit.
    let size_output = git.run(&["cat-file", "-s", &object_spec]).await?;

    if !size_output.success() {
        let stderr = &size_output.stderr;
        if stderr.contains("Not a valid object name")
            || stderr.contains("does not exist")
            || stderr.contains("unknown revision")
            || stderr.contains("bad file")
        {
            return Err(ApiError::NotFound(format!(
                "file '{}' not found at ref '{}'",
                path, ref_name
            )));
        }
        return Err(ApiError::Internal("failed to get file size".to_string()));
    }

    let size_str = String::from_utf8_lossy(&size_output.stdout);
    let size: u64 = size_str.trim().parse().unwrap_or(0);

    if size > MAX_FILE_SIZE {
        return Err(ApiError::Unprocessable(format!(
            "file is too large ({} bytes, max {} bytes)",
            size, MAX_FILE_SIZE
        )));
    }

    // Retrieve the file content.
    let output = git.run(&["show", &object_spec]).await?;

    if !output.success() {
        let stderr = &output.stderr;
        if stderr.contains("does not exist")
            || stderr.contains("Not a valid object name")
            || stderr.contains("unknown revision")
        {
            return Err(ApiError::NotFound(format!(
                "file '{}' not found at ref '{}'",
                path, ref_name
            )));
        }
        return Err(ApiError::Internal("failed to get file content".to_string()));
    }

    let raw_bytes = &output.stdout;
    let is_binary = is_binary_content(raw_bytes);

    let content = if is_binary {
        base64::engine::general_purpose::STANDARD.encode(raw_bytes)
    } else {
        String::from_utf8_lossy(raw_bytes).into_owned()
    };

    Ok(FileContent {
        content,
        size,
        is_binary,
    })
}

/// Heuristic to detect binary content.
///
/// A file is considered binary if the first 8 KB contain a NUL byte.
fn is_binary_content(data: &[u8]) -> bool {
    let check_len = std::cmp::min(data.len(), 8192);
    data[..check_len].contains(&0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_storage(dir: &std::path::Path) -> StorageConfig {
        StorageConfig::new(dir.to_path_buf())
    }

    /// Create a bare repo and add an initial commit so we can query history.
    async fn setup_repo_with_commit(storage: &StorageConfig, repo_id: Uuid) -> PathBuf {
        let path = crate::storage::service::init_bare_repo(storage, repo_id, "main")
            .await
            .expect("failed to init bare repo");

        let tmp_clone = path.parent().unwrap().join("tmp-clone");
        tokio::fs::create_dir_all(&tmp_clone)
            .await
            .expect("create tmp clone dir");

        let output = tokio::process::Command::new("git")
            .args(["clone", path.to_str().unwrap(), tmp_clone.to_str().unwrap()])
            .output()
            .await
            .expect("git clone");
        assert!(
            output.status.success() || String::from_utf8_lossy(&output.stderr).contains("empty")
        );

        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config email");

        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config name");

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

        let push_output = tokio::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git push");

        if !push_output.status.success() {
            tokio::process::Command::new("git")
                .args(["push", "origin", "HEAD:refs/heads/main"])
                .current_dir(&tmp_clone)
                .output()
                .await
                .expect("git push HEAD:main");
        }

        let _ = tokio::fs::remove_dir_all(&tmp_clone).await;

        path
    }

    /// Create a repo with two commits (initial + a modification).
    async fn setup_repo_with_two_commits(storage: &StorageConfig, repo_id: Uuid) -> PathBuf {
        let path = crate::storage::service::init_bare_repo(storage, repo_id, "main")
            .await
            .expect("failed to init bare repo");

        let tmp_clone = path.parent().unwrap().join("tmp-clone2");
        tokio::fs::create_dir_all(&tmp_clone)
            .await
            .expect("create tmp clone dir");

        let output = tokio::process::Command::new("git")
            .args(["clone", path.to_str().unwrap(), tmp_clone.to_str().unwrap()])
            .output()
            .await
            .expect("git clone");
        assert!(
            output.status.success() || String::from_utf8_lossy(&output.stderr).contains("empty")
        );

        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config email");

        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config name");

        // First commit
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

        // Second commit
        tokio::fs::write(tmp_clone.join("README.md"), "# Test\nUpdated\n")
            .await
            .expect("write readme again");

        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git add 2");

        tokio::process::Command::new("git")
            .args(["commit", "-m", "update readme"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git commit 2");

        let push_output = tokio::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git push");

        if !push_output.status.success() {
            tokio::process::Command::new("git")
                .args(["push", "origin", "HEAD:refs/heads/main"])
                .current_dir(&tmp_clone)
                .output()
                .await
                .expect("git push HEAD:main");
        }

        let _ = tokio::fs::remove_dir_all(&tmp_clone).await;

        path
    }

    #[test]
    fn parse_commit_record_valid() {
        let record = "abc123def456abc123def456abc123def456abcd1234\n\
                       Alice\n\
                       alice@example.com\n\
                       Bob\n\
                       bob@example.com\n\
                       initial commit\n\
                       2024-01-15T10:30:00+00:00\n\
                       parent1 parent2";

        let commit = parse_commit_record(record).unwrap();
        assert_eq!(commit.sha, "abc123def456abc123def456abc123def456abcd1234");
        assert_eq!(commit.author_name, "Alice");
        assert_eq!(commit.author_email, "alice@example.com");
        assert_eq!(commit.committer_name, "Bob");
        assert_eq!(commit.committer_email, "bob@example.com");
        assert_eq!(commit.message, "initial commit");
        assert_eq!(commit.parent_shas, vec!["parent1", "parent2"]);
    }

    #[test]
    fn parse_commit_record_root_commit() {
        let record = "abc123def456abc123def456abc123def456abcd1234\n\
                       Alice\n\
                       alice@example.com\n\
                       Alice\n\
                       alice@example.com\n\
                       root commit\n\
                       2024-01-15T10:30:00+00:00\n";

        let commit = parse_commit_record(record).unwrap();
        assert!(commit.parent_shas.is_empty());
    }

    #[test]
    fn parse_commit_record_too_short() {
        let record = "abc123\nAlice\nalice@example.com";
        assert!(parse_commit_record(record).is_none());
    }

    #[tokio::test]
    async fn list_commits_empty_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        crate::storage::service::init_bare_repo(&storage, id, "main")
            .await
            .expect("init");

        let commits = list_commits(&storage, id, "main", 30, 0).await.unwrap();
        assert!(commits.is_empty());
    }

    #[tokio::test]
    async fn list_commits_with_history() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let commits = list_commits(&storage, id, "main", 30, 0).await.unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].message, "initial commit");
        assert_eq!(commits[0].author_name, "Test User");
        assert_eq!(commits[0].author_email, "test@test.com");
        assert!(commits[0].parent_shas.is_empty()); // root commit
        assert!(!commits[0].sha.is_empty());
        assert!(commits[0].sha.len() >= 40);
    }

    #[tokio::test]
    async fn list_commits_pagination() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_two_commits(&storage, id).await;

        // Get only the first commit (most recent)
        let page1 = list_commits(&storage, id, "main", 1, 0).await.unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].message, "update readme");

        // Get the second commit
        let page2 = list_commits(&storage, id, "main", 1, 1).await.unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].message, "initial commit");

        // Offset past all commits
        let page3 = list_commits(&storage, id, "main", 1, 10).await.unwrap();
        assert!(page3.is_empty());
    }

    #[tokio::test]
    async fn list_commits_invalid_ref() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let commits = list_commits(&storage, id, "nonexistent-branch", 30, 0)
            .await
            .unwrap();
        assert!(commits.is_empty());
    }

    #[tokio::test]
    async fn get_commit_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        // First get the SHA from the log
        let commits = list_commits(&storage, id, "main", 1, 0).await.unwrap();
        assert_eq!(commits.len(), 1);
        let sha = &commits[0].sha;

        let commit = get_commit(&storage, id, sha).await.unwrap();
        assert!(commit.is_some());
        let c = commit.unwrap();
        assert_eq!(c.sha, *sha);
        assert_eq!(c.message, "initial commit");
        assert_eq!(c.author_name, "Test User");
    }

    #[tokio::test]
    async fn get_commit_nonexistent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let commit = get_commit(&storage, id, "0000000000000000000000000000000000000000")
            .await
            .unwrap();
        assert!(commit.is_none());
    }

    #[tokio::test]
    async fn get_commit_diff_initial_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let commits = list_commits(&storage, id, "main", 1, 0).await.unwrap();
        let sha = &commits[0].sha;

        let diff = get_commit_diff(&storage, id, sha).await.unwrap();
        // Initial commit adds README.md
        assert!(!diff.is_empty());
        let readme_entry = diff.iter().find(|e| e.path == "README.md");
        assert!(readme_entry.is_some());
        let entry = readme_entry.unwrap();
        assert_eq!(entry.status, DiffStatus::Added);
        assert!(entry.additions > 0);
    }

    #[tokio::test]
    async fn get_commit_diff_modification() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_two_commits(&storage, id).await;

        // Get the most recent commit (the modification)
        let commits = list_commits(&storage, id, "main", 1, 0).await.unwrap();
        let sha = &commits[0].sha;
        assert_eq!(commits[0].message, "update readme");

        let diff = get_commit_diff(&storage, id, sha).await.unwrap();
        assert!(!diff.is_empty());
        let readme_entry = diff.iter().find(|e| e.path == "README.md");
        assert!(readme_entry.is_some());
        let entry = readme_entry.unwrap();
        assert_eq!(entry.status, DiffStatus::Modified);
    }

    #[tokio::test]
    async fn get_commit_diff_nonexistent_sha() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_commit(&storage, id).await;

        let result =
            get_commit_diff(&storage, id, "0000000000000000000000000000000000000000").await;
        assert!(result.is_err());
    }

    /// Create a repo with a directory structure for tree browsing tests.
    /// Structure:
    ///   README.md
    ///   src/
    ///     main.rs
    ///     lib.rs
    async fn setup_repo_with_tree(storage: &StorageConfig, repo_id: Uuid) -> PathBuf {
        let path = crate::storage::service::init_bare_repo(storage, repo_id, "main")
            .await
            .expect("failed to init bare repo");

        let tmp_clone = path.parent().unwrap().join("tmp-clone-tree");
        tokio::fs::create_dir_all(&tmp_clone)
            .await
            .expect("create tmp clone dir");

        let output = tokio::process::Command::new("git")
            .args(["clone", path.to_str().unwrap(), tmp_clone.to_str().unwrap()])
            .output()
            .await
            .expect("git clone");
        assert!(
            output.status.success() || String::from_utf8_lossy(&output.stderr).contains("empty")
        );

        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config email");

        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config name");

        // Create files
        tokio::fs::write(tmp_clone.join("README.md"), "# Hello\n")
            .await
            .expect("write readme");

        tokio::fs::create_dir_all(tmp_clone.join("src"))
            .await
            .expect("create src dir");

        tokio::fs::write(tmp_clone.join("src/main.rs"), "fn main() {}\n")
            .await
            .expect("write main.rs");

        tokio::fs::write(tmp_clone.join("src/lib.rs"), "pub fn hello() {}\n")
            .await
            .expect("write lib.rs");

        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git add");

        tokio::process::Command::new("git")
            .args(["commit", "-m", "add files"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git commit");

        let push_output = tokio::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git push");

        if !push_output.status.success() {
            tokio::process::Command::new("git")
                .args(["push", "origin", "HEAD:refs/heads/main"])
                .current_dir(&tmp_clone)
                .output()
                .await
                .expect("git push HEAD:main");
        }

        let _ = tokio::fs::remove_dir_all(&tmp_clone).await;

        path
    }

    // -----------------------------------------------------------------------
    // list_tree tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_tree_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let entries = list_tree(&storage, id, "main", None).await.unwrap();

        // Root should contain README.md (blob) and src (tree)
        assert_eq!(entries.len(), 2);

        let readme = entries.iter().find(|e| e.name == "README.md");
        assert!(readme.is_some());
        let readme = readme.unwrap();
        assert_eq!(readme.entry_type, TreeEntryType::Blob);
        assert_eq!(readme.path, "README.md");
        assert!(readme.size.is_some());
        assert!(readme.size.unwrap() > 0);
        assert!(!readme.sha.is_empty());

        let src = entries.iter().find(|e| e.name == "src");
        assert!(src.is_some());
        let src = src.unwrap();
        assert_eq!(src.entry_type, TreeEntryType::Tree);
        assert_eq!(src.path, "src");
        assert!(src.size.is_none());
    }

    #[tokio::test]
    async fn list_tree_subdirectory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let entries = list_tree(&storage, id, "main", Some("src")).await.unwrap();

        // src/ should contain main.rs and lib.rs
        assert_eq!(entries.len(), 2);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"main.rs"));
        assert!(names.contains(&"lib.rs"));

        // Paths should include the prefix
        for entry in &entries {
            assert!(entry.path.starts_with("src/"));
            assert_eq!(entry.entry_type, TreeEntryType::Blob);
            assert!(entry.size.is_some());
        }
    }

    #[tokio::test]
    async fn list_tree_invalid_ref() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let result = list_tree(&storage, id, "nonexistent-branch", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_tree_invalid_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let result = list_tree(&storage, id, "main", Some("nonexistent")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_tree_empty_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        crate::storage::service::init_bare_repo(&storage, id, "main")
            .await
            .expect("init");

        let result = list_tree(&storage, id, "main", None).await;
        // Empty repo has no tree, should return an error (not found)
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // get_file_content tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_file_content_text_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let fc = get_file_content(&storage, id, "main", "README.md")
            .await
            .unwrap();

        assert_eq!(fc.content, "# Hello\n");
        assert!(!fc.is_binary);
        assert!(fc.size > 0);
    }

    #[tokio::test]
    async fn get_file_content_nested_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let fc = get_file_content(&storage, id, "main", "src/main.rs")
            .await
            .unwrap();

        assert_eq!(fc.content, "fn main() {}\n");
        assert!(!fc.is_binary);
        assert!(fc.size > 0);
    }

    #[tokio::test]
    async fn get_file_content_nonexistent_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let result = get_file_content(&storage, id, "main", "nonexistent.txt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_file_content_nonexistent_ref() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        setup_repo_with_tree(&storage, id).await;

        let result = get_file_content(&storage, id, "nonexistent-branch", "README.md").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_file_content_binary_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(tmp.path());
        let id = Uuid::new_v4();

        let path = crate::storage::service::init_bare_repo(&storage, id, "main")
            .await
            .expect("failed to init bare repo");

        let tmp_clone = path.parent().unwrap().join("tmp-clone-bin");
        tokio::fs::create_dir_all(&tmp_clone)
            .await
            .expect("create tmp clone dir");

        let output = tokio::process::Command::new("git")
            .args(["clone", path.to_str().unwrap(), tmp_clone.to_str().unwrap()])
            .output()
            .await
            .expect("git clone");
        assert!(
            output.status.success() || String::from_utf8_lossy(&output.stderr).contains("empty")
        );

        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config email");

        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git config name");

        // Write a file with NUL bytes (binary)
        let binary_data: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x00, 0x01, 0x02, 0x03];
        tokio::fs::write(tmp_clone.join("image.bin"), &binary_data)
            .await
            .expect("write binary file");

        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git add");

        tokio::process::Command::new("git")
            .args(["commit", "-m", "add binary file"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git commit");

        let push_output = tokio::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&tmp_clone)
            .output()
            .await
            .expect("git push");

        if !push_output.status.success() {
            tokio::process::Command::new("git")
                .args(["push", "origin", "HEAD:refs/heads/main"])
                .current_dir(&tmp_clone)
                .output()
                .await
                .expect("git push HEAD:main");
        }

        let _ = tokio::fs::remove_dir_all(&tmp_clone).await;

        let fc = get_file_content(&storage, id, "main", "image.bin")
            .await
            .unwrap();

        assert!(fc.is_binary);
        assert!(fc.size > 0);

        // Content should be base64 encoded
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&fc.content)
            .expect("should be valid base64");
        assert_eq!(decoded, binary_data);
    }

    #[test]
    fn is_binary_content_detects_nul_bytes() {
        assert!(is_binary_content(&[0x00, 0x01, 0x02]));
        assert!(is_binary_content(&[0x48, 0x65, 0x6C, 0x00]));
    }

    #[test]
    fn is_binary_content_text_is_not_binary() {
        assert!(!is_binary_content(b"hello world\n"));
        assert!(!is_binary_content(b"fn main() {}\n"));
    }

    #[test]
    fn is_binary_content_empty_is_not_binary() {
        assert!(!is_binary_content(b""));
    }
}

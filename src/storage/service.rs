use std::path::PathBuf;

use uuid::Uuid;

use crate::errors::ApiError;

/// Configuration for the on-disk Git storage layer.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Root directory under which all bare repositories are stored.
    pub root_path: PathBuf,
}

impl StorageConfig {
    /// Create a new `StorageConfig` with the given root path.
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
    }
}

/// Derive the on-disk path for a bare repository from its UUID.
///
/// Layout: `{root}/{first_2_chars_of_uuid}/{uuid}.git`
///
/// The first two hex characters of the UUID are used as a fan-out prefix
/// directory to avoid putting too many entries in a single directory.
///
/// Only the UUID (which is system-generated) is used to derive the path,
/// making path traversal impossible.
pub fn repo_path(config: &StorageConfig, repo_id: Uuid) -> PathBuf {
    let id_str = repo_id.to_string();
    let prefix = &id_str[..2];
    config
        .root_path
        .join(prefix)
        .join(format!("{}.git", id_str))
}

/// Initialize a bare Git repository on disk for the given repo ID.
///
/// 1. Creates the parent (fan-out) directory if needed.
/// 2. Runs `git init --bare {path}`.
/// 3. Sets HEAD to `refs/heads/{default_branch}` via `git symbolic-ref`.
///
/// If the repository directory already exists, this is a no-op (idempotent).
#[allow(dead_code)]
pub async fn init_bare_repo(
    config: &StorageConfig,
    repo_id: Uuid,
    default_branch: &str,
) -> Result<PathBuf, ApiError> {
    let path = repo_path(config, repo_id);

    if path.exists() {
        tracing::warn!(
            path = %path.display(),
            "bare repo directory already exists, skipping init"
        );
        return Ok(path);
    }

    // Create parent directories (the fan-out prefix dir).
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            tracing::error!(
                error = %e,
                path = %parent.display(),
                "failed to create parent directory for bare repo"
            );
            ApiError::Internal("failed to initialize repository storage".to_string())
        })?;
    }

    // Run `git init --bare`.
    let path_clone = path.clone();
    let output = tokio::process::Command::new("git")
        .arg("init")
        .arg("--bare")
        .arg(&path_clone)
        .output()
        .await
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

    // Set HEAD to the configured default branch.
    let head_ref = format!("refs/heads/{}", default_branch);
    let output = tokio::process::Command::new("git")
        .arg("symbolic-ref")
        .arg("HEAD")
        .arg(&head_ref)
        .env("GIT_DIR", &path)
        .output()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to execute git symbolic-ref HEAD");
            ApiError::Internal("failed to set default branch".to_string())
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(stderr = %stderr, "git symbolic-ref HEAD failed");
        return Err(ApiError::Internal(
            "failed to set default branch".to_string(),
        ));
    }

    tracing::info!(
        repo_id = %repo_id,
        path = %path.display(),
        default_branch = %default_branch,
        "initialized bare git repository"
    );

    Ok(path)
}

/// Check whether a bare repository exists on disk for the given repo ID.
///
/// Returns `true` if the derived path exists and is a directory.
pub async fn repo_exists(config: &StorageConfig, repo_id: Uuid) -> bool {
    let path = repo_path(config, repo_id);
    match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.is_dir(),
        Err(_) => false,
    }
}

/// Delete a bare repository from disk by removing its directory recursively.
pub async fn delete_repo(config: &StorageConfig, repo_id: Uuid) -> Result<(), ApiError> {
    let path = repo_path(config, repo_id);

    if !path.exists() {
        tracing::warn!(
            repo_id = %repo_id,
            path = %path.display(),
            "repo directory does not exist, nothing to delete"
        );
        return Ok(());
    }

    tokio::fs::remove_dir_all(&path).await.map_err(|e| {
        tracing::error!(
            error = %e,
            path = %path.display(),
            "failed to delete repo directory"
        );
        ApiError::Internal("failed to delete repository from disk".to_string())
    })?;

    tracing::info!(
        repo_id = %repo_id,
        path = %path.display(),
        "deleted bare git repository from disk"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(dir: &std::path::Path) -> StorageConfig {
        StorageConfig::new(dir.to_path_buf())
    }

    #[test]
    fn repo_path_uses_fanout_prefix() {
        let config = StorageConfig::new(PathBuf::from("/data/repos"));
        let id = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let path = repo_path(&config, id);
        assert_eq!(
            path,
            PathBuf::from("/data/repos/a1/a1b2c3d4-e5f6-7890-abcd-ef1234567890.git")
        );
    }

    #[test]
    fn repo_path_different_ids_get_different_prefixes() {
        let config = StorageConfig::new(PathBuf::from("/data/repos"));
        let id1 = Uuid::parse_str("abcdef01-0000-0000-0000-000000000000").unwrap();
        let id2 = Uuid::parse_str("12345678-0000-0000-0000-000000000000").unwrap();

        let p1 = repo_path(&config, id1);
        let p2 = repo_path(&config, id2);

        assert!(p1.starts_with("/data/repos/ab/"));
        assert!(p2.starts_with("/data/repos/12/"));
        assert_ne!(p1, p2);
    }

    #[test]
    fn repo_path_only_uuid_in_path() {
        // Ensures no user-supplied strings leak into the path.
        let config = StorageConfig::new(PathBuf::from("/srv/git"));
        let id = Uuid::new_v4();
        let path = repo_path(&config, id);
        let id_str = id.to_string();

        // The path should be exactly root/prefix/uuid.git
        let expected = PathBuf::from(format!("/srv/git/{}/{}.git", &id_str[..2], id_str));
        assert_eq!(path, expected);
    }

    #[tokio::test]
    async fn init_bare_repo_creates_repo() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let config = test_config(tmp.path());
        let id = Uuid::new_v4();

        let result = init_bare_repo(&config, id, "main").await;
        assert!(result.is_ok());

        let path = result.unwrap();
        assert!(path.exists());
        assert!(path.is_dir());
        // Should contain HEAD file (bare repo marker)
        assert!(path.join("HEAD").exists());
    }

    #[tokio::test]
    async fn init_bare_repo_sets_default_branch() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let config = test_config(tmp.path());
        let id = Uuid::new_v4();

        let path = init_bare_repo(&config, id, "develop").await.unwrap();

        // Read the HEAD file to verify symbolic-ref was set.
        let head_content = tokio::fs::read_to_string(path.join("HEAD"))
            .await
            .expect("failed to read HEAD");
        assert!(
            head_content.contains("refs/heads/develop"),
            "HEAD should reference refs/heads/develop, got: {}",
            head_content
        );
    }

    #[tokio::test]
    async fn init_bare_repo_is_idempotent() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let config = test_config(tmp.path());
        let id = Uuid::new_v4();

        let path1 = init_bare_repo(&config, id, "main").await.unwrap();
        let path2 = init_bare_repo(&config, id, "main").await.unwrap();
        assert_eq!(path1, path2);
    }

    #[tokio::test]
    async fn repo_exists_returns_true_for_existing() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let config = test_config(tmp.path());
        let id = Uuid::new_v4();

        assert!(!repo_exists(&config, id).await);
        init_bare_repo(&config, id, "main").await.unwrap();
        assert!(repo_exists(&config, id).await);
    }

    #[tokio::test]
    async fn repo_exists_returns_false_for_missing() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let config = test_config(tmp.path());
        let id = Uuid::new_v4();

        assert!(!repo_exists(&config, id).await);
    }

    #[tokio::test]
    async fn delete_repo_removes_directory() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let config = test_config(tmp.path());
        let id = Uuid::new_v4();

        init_bare_repo(&config, id, "main").await.unwrap();
        assert!(repo_exists(&config, id).await);

        delete_repo(&config, id).await.unwrap();
        assert!(!repo_exists(&config, id).await);
    }

    #[tokio::test]
    async fn delete_repo_noop_if_not_exists() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let config = test_config(tmp.path());
        let id = Uuid::new_v4();

        // Should succeed even if repo does not exist.
        let result = delete_repo(&config, id).await;
        assert!(result.is_ok());
    }

}

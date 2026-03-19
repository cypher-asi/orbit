use std::path::PathBuf;

use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::ApiError;
use crate::repos::models::Repo;
use crate::repos::service as repo_service;

/// Resolve a Git repository from URL path components.
///
/// Strips a trailing `.git` suffix from `repo_slug` if present, then looks
/// up the repo by `(org_id, slug)`.
///
/// Returns the `Repo` record and the on-disk bare repository path.
///
/// # Errors
///
/// Returns `ApiError::NotFound` (with a generic message) if the repo does
/// not exist.
pub async fn resolve_git_repo(
    pool: &PgPool,
    storage_root: &std::path::Path,
    org_id: Uuid,
    repo_slug: &str,
) -> Result<(Repo, PathBuf), ApiError> {
    // Strip trailing .git suffix if present.
    let slug = repo_slug.strip_suffix(".git").unwrap_or(repo_slug);

    // Look up the repo by org_id + slug.
    let repo = repo_service::get_repo_by_org_and_slug(pool, org_id, slug)
        .await?
        .ok_or_else(|| ApiError::NotFound("repository not found".to_string()))?;

    // Derive the on-disk path. The storage layer uses a fan-out prefix:
    //   {root}/{first_2_chars_of_uuid}/{uuid}.git
    let id_str = repo.id.to_string();
    let prefix = &id_str[..2];
    let disk_path = storage_root.join(prefix).join(format!("{}.git", id_str));

    Ok((repo, disk_path))
}

/// Format a pkt-line string.
///
/// Git pkt-line format: 4 hex chars of total length (including the 4 length
/// bytes) followed by the payload. E.g., `001e# service=git-upload-pack\n`.
pub fn pkt_line(data: &str) -> Vec<u8> {
    let total_len = 4 + data.len();
    format!("{:04x}{}", total_len, data).into_bytes()
}

/// The pkt-line flush packet (`0000`).
pub fn pkt_flush() -> Vec<u8> {
    b"0000".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkt_line_formats_correctly() {
        let line = pkt_line("# service=git-upload-pack\n");
        let s = String::from_utf8(line).unwrap();
        // 4 (length) + 26 (payload) = 30 = 0x1e
        assert_eq!(s, "001e# service=git-upload-pack\n");
    }

    #[test]
    fn pkt_flush_is_0000() {
        let flush = pkt_flush();
        assert_eq!(flush, b"0000");
    }

    #[test]
    fn pkt_line_short_string() {
        let line = pkt_line("hi\n");
        let s = String::from_utf8(line).unwrap();
        // 4 + 3 = 7 = 0x0007
        assert_eq!(s, "0007hi\n");
    }

    #[test]
    fn strip_git_suffix() {
        let slug = "my-repo.git";
        let stripped = slug.strip_suffix(".git").unwrap_or(slug);
        assert_eq!(stripped, "my-repo");
    }

    #[test]
    fn no_git_suffix() {
        let slug = "my-repo";
        let stripped = slug.strip_suffix(".git").unwrap_or(slug);
        assert_eq!(stripped, "my-repo");
    }
}

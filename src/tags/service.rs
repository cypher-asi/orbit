use uuid::Uuid;

use crate::errors::ApiError;
use crate::storage::git::GitCommand;
use crate::storage::service::{repo_path, StorageConfig};

use super::models::TagInfo;

/// List all tags in a repository.
///
/// Runs `git for-each-ref --format='%(refname:short) %(objectname) %(*objectname)' refs/tags/`
/// and parses the output. Returns tags in refname order (typically lexicographic).
/// For pagination, the caller should slice the returned Vec (e.g. [offset..][..limit]).
pub async fn list_tags(
    storage: &StorageConfig,
    repo_id: Uuid,
) -> Result<Vec<TagInfo>, ApiError> {
    let path = repo_path(storage, repo_id);
    let git = GitCommand::new(path);

    // refname:short = tag name, objectname = target SHA, *objectname = peeled (for annotated tags)
    let output = git
        .run(&[
            "for-each-ref",
            "--format=%(refname:short) %(objectname) %(*objectname)",
            "refs/tags/",
        ])
        .await?;

    if !output.success() {
        tracing::error!(stderr = %output.stderr, "git for-each-ref refs/tags failed");
        return Err(ApiError::Internal(
            "failed to list tags".to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tags = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(3, char::is_whitespace).collect();
        if parts.len() < 2 {
            continue;
        }

        let name = parts[0].to_string();
        let target = parts[1].to_string();
        let peeled = parts
            .get(2)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        tags.push(TagInfo {
            name,
            target,
            peeled,
        });
    }

    Ok(tags)
}

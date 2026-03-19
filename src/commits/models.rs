use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Information about a Git commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Full commit SHA.
    pub sha: String,
    /// Author name.
    pub author_name: String,
    /// Author email.
    pub author_email: String,
    /// Committer name.
    pub committer_name: String,
    /// Committer email.
    pub committer_email: String,
    /// Commit message (subject + body).
    pub message: String,
    /// Author timestamp.
    pub timestamp: DateTime<Utc>,
    /// Parent commit SHAs.
    pub parent_shas: Vec<String>,
}

/// Type of entry in a Git tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TreeEntryType {
    Blob,
    Tree,
}

/// An entry in a Git tree listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeEntry {
    /// File or directory name.
    pub name: String,
    /// Full path relative to repository root.
    pub path: String,
    /// Whether this is a blob (file) or tree (directory).
    pub entry_type: TreeEntryType,
    /// Object SHA.
    pub sha: String,
    /// Size in bytes (only for blobs).
    pub size: Option<u64>,
}

/// Contents of a file at a given ref and path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    /// File content as a string (or base64 for binary).
    pub content: String,
    /// Size in bytes.
    pub size: u64,
    /// Whether the file is binary.
    pub is_binary: bool,
}

/// Status of a file in a commit diff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiffStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// A changed file in a commit diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffEntry {
    /// File path.
    pub path: String,
    /// Change status.
    pub status: DiffStatus,
    /// Number of lines added.
    pub additions: u32,
    /// Number of lines deleted.
    pub deletions: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_info_serializes() {
        let info = CommitInfo {
            sha: "abc123".to_string(),
            author_name: "Alice".to_string(),
            author_email: "alice@example.com".to_string(),
            committer_name: "Bob".to_string(),
            committer_email: "bob@example.com".to_string(),
            message: "initial commit".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            parent_shas: vec!["def456".to_string()],
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["sha"], "abc123");
        assert_eq!(json["author_name"], "Alice");
        assert_eq!(json["parent_shas"][0], "def456");
    }

    #[test]
    fn tree_entry_type_serializes() {
        let blob = TreeEntryType::Blob;
        let tree = TreeEntryType::Tree;
        assert_eq!(serde_json::to_value(&blob).unwrap(), "blob");
        assert_eq!(serde_json::to_value(&tree).unwrap(), "tree");
    }

    #[test]
    fn diff_status_serializes() {
        assert_eq!(serde_json::to_value(&DiffStatus::Added).unwrap(), "added");
        assert_eq!(serde_json::to_value(&DiffStatus::Modified).unwrap(), "modified");
        assert_eq!(serde_json::to_value(&DiffStatus::Deleted).unwrap(), "deleted");
        assert_eq!(serde_json::to_value(&DiffStatus::Renamed).unwrap(), "renamed");
    }

    #[test]
    fn file_content_serializes() {
        let fc = FileContent {
            content: "hello world".to_string(),
            size: 11,
            is_binary: false,
        };
        let json = serde_json::to_value(&fc).unwrap();
        assert_eq!(json["content"], "hello world");
        assert_eq!(json["size"], 11);
        assert_eq!(json["is_binary"], false);
    }

    #[test]
    fn diff_entry_serializes() {
        let entry = DiffEntry {
            path: "src/main.rs".to_string(),
            status: DiffStatus::Modified,
            additions: 10,
            deletions: 3,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["path"], "src/main.rs");
        assert_eq!(json["status"], "modified");
        assert_eq!(json["additions"], 10);
        assert_eq!(json["deletions"], 3);
    }
}

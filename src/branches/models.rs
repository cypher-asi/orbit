use serde::{Deserialize, Serialize};

/// Information about a Git branch in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    /// Branch name (e.g. "main", "feature/foo").
    pub name: String,
    /// The SHA of the commit at the tip of this branch.
    pub head_commit: String,
    /// Whether this branch is the repository's default branch.
    pub is_default: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_info_serializes() {
        let info = BranchInfo {
            name: "main".to_string(),
            head_commit: "abc123".to_string(),
            is_default: true,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["name"], "main");
        assert_eq!(json["head_commit"], "abc123");
        assert_eq!(json["is_default"], true);
    }

    #[test]
    fn branch_info_deserializes() {
        let json = r#"{"name":"develop","head_commit":"def456","is_default":false}"#;
        let info: BranchInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.name, "develop");
        assert_eq!(info.head_commit, "def456");
        assert!(!info.is_default);
    }

    #[test]
    fn branch_info_clone() {
        let info = BranchInfo {
            name: "main".to_string(),
            head_commit: "abc123".to_string(),
            is_default: true,
        };
        let cloned = info.clone();
        assert_eq!(cloned.name, info.name);
        assert_eq!(cloned.head_commit, info.head_commit);
        assert_eq!(cloned.is_default, info.is_default);
    }
}

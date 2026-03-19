use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MergeStrategy
// ---------------------------------------------------------------------------

/// The merge strategy to use when merging a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Creates a merge commit with two parents (target HEAD + source HEAD).
    MergeCommit,
    /// Squashes all source commits into a single commit on the target branch.
    Squash,
    /// Rebases source commits onto the target branch tip, then fast-forwards.
    RebaseAndMerge,
}

impl MergeStrategy {
    /// Return a human-readable string for the strategy.
    pub fn as_str(&self) -> &'static str {
        match self {
            MergeStrategy::MergeCommit => "merge_commit",
            MergeStrategy::Squash => "squash",
            MergeStrategy::RebaseAndMerge => "rebase_and_merge",
        }
    }
}

impl std::fmt::Display for MergeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// MergeResult
// ---------------------------------------------------------------------------

/// The result of a successful merge operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeResult {
    /// SHA of the merge commit (or the resulting tip commit for squash/rebase).
    pub merge_commit_sha: String,
    /// The strategy that was used.
    pub strategy: MergeStrategy,
    /// Timestamp when the merge completed.
    pub merged_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// MergeRequest
// ---------------------------------------------------------------------------

/// Input body for a merge request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeRequest {
    /// The merge strategy to use.
    pub strategy: MergeStrategy,
    /// Optional custom commit message for the merge.
    pub commit_message: Option<String>,
}

// ---------------------------------------------------------------------------
// ConflictCheck
// ---------------------------------------------------------------------------

/// The result of a conflict check between two branches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictCheck {
    /// Whether there are merge conflicts.
    pub has_conflicts: bool,
    /// List of files that have conflicts (empty if no conflicts).
    pub conflicting_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- MergeStrategy serde tests ------------------------------------------

    #[test]
    fn merge_strategy_serialize() {
        let json = serde_json::to_string(&MergeStrategy::MergeCommit).unwrap();
        assert_eq!(json, r#""merge_commit""#);

        let json = serde_json::to_string(&MergeStrategy::Squash).unwrap();
        assert_eq!(json, r#""squash""#);

        let json = serde_json::to_string(&MergeStrategy::RebaseAndMerge).unwrap();
        assert_eq!(json, r#""rebase_and_merge""#);
    }

    #[test]
    fn merge_strategy_deserialize() {
        let v: MergeStrategy = serde_json::from_str(r#""merge_commit""#).unwrap();
        assert_eq!(v, MergeStrategy::MergeCommit);

        let v: MergeStrategy = serde_json::from_str(r#""squash""#).unwrap();
        assert_eq!(v, MergeStrategy::Squash);

        let v: MergeStrategy = serde_json::from_str(r#""rebase_and_merge""#).unwrap();
        assert_eq!(v, MergeStrategy::RebaseAndMerge);
    }

    #[test]
    fn merge_strategy_round_trip() {
        for strategy in &[
            MergeStrategy::MergeCommit,
            MergeStrategy::Squash,
            MergeStrategy::RebaseAndMerge,
        ] {
            let json = serde_json::to_string(strategy).unwrap();
            let parsed: MergeStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(*strategy, parsed);
        }
    }

    #[test]
    fn merge_strategy_as_str() {
        assert_eq!(MergeStrategy::MergeCommit.as_str(), "merge_commit");
        assert_eq!(MergeStrategy::Squash.as_str(), "squash");
        assert_eq!(MergeStrategy::RebaseAndMerge.as_str(), "rebase_and_merge");
    }

    #[test]
    fn merge_strategy_display() {
        assert_eq!(MergeStrategy::MergeCommit.to_string(), "merge_commit");
        assert_eq!(MergeStrategy::Squash.to_string(), "squash");
        assert_eq!(
            MergeStrategy::RebaseAndMerge.to_string(),
            "rebase_and_merge"
        );
    }

    // -- MergeResult tests --------------------------------------------------

    #[test]
    fn merge_result_serialize() {
        let result = MergeResult {
            merge_commit_sha: "abc123".to_string(),
            strategy: MergeStrategy::MergeCommit,
            merged_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["mergeCommitSha"], "abc123");
        assert_eq!(json["strategy"], "merge_commit");
        assert!(json["mergedAt"].is_string());
    }

    // -- MergeRequest tests -------------------------------------------------

    #[test]
    fn merge_request_deserialize_full() {
        let json = r#"{
            "strategy": "squash",
            "commitMessage": "Squash all the things"
        }"#;
        let req: MergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.strategy, MergeStrategy::Squash);
        assert_eq!(req.commit_message.as_deref(), Some("Squash all the things"));
    }

    #[test]
    fn merge_request_deserialize_no_message() {
        let json = r#"{"strategy": "merge_commit"}"#;
        let req: MergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.strategy, MergeStrategy::MergeCommit);
        assert!(req.commit_message.is_none());
    }

    #[test]
    fn merge_request_deserialize_rebase() {
        let json = r#"{"strategy": "rebase_and_merge"}"#;
        let req: MergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.strategy, MergeStrategy::RebaseAndMerge);
    }

    #[test]
    fn merge_request_invalid_strategy() {
        let json = r#"{"strategy": "invalid"}"#;
        let result = serde_json::from_str::<MergeRequest>(json);
        assert!(result.is_err());
    }

    // -- ConflictCheck tests ------------------------------------------------

    #[test]
    fn conflict_check_no_conflicts() {
        let check = ConflictCheck {
            has_conflicts: false,
            conflicting_files: vec![],
        };
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json["hasConflicts"], false);
        assert_eq!(json["conflictingFiles"], serde_json::json!([]));
    }

    #[test]
    fn conflict_check_with_conflicts() {
        let check = ConflictCheck {
            has_conflicts: true,
            conflicting_files: vec!["file1.rs".to_string(), "file2.rs".to_string()],
        };
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json["hasConflicts"], true);
        assert_eq!(
            json["conflictingFiles"],
            serde_json::json!(["file1.rs", "file2.rs"])
        );
    }

    #[test]
    fn conflict_check_deserialize() {
        let json = r#"{"hasConflicts": true, "conflictingFiles": ["a.txt"]}"#;
        let check: ConflictCheck = serde_json::from_str(json).unwrap();
        assert!(check.has_conflicts);
        assert_eq!(check.conflicting_files, vec!["a.txt"]);
    }
}

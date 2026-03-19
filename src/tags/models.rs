use serde::{Deserialize, Serialize};

/// Information about a Git tag in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagInfo {
    /// Tag name (e.g. "v1.0.0").
    pub name: String,
    /// The SHA of the object the tag points to (commit for lightweight, tag for annotated).
    pub target: String,
    /// For annotated tags, the SHA of the peeled (dereferenced) commit. Omitted for lightweight tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peeled: Option<String>,
}

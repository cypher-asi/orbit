//! Feed post: auto-create a push post in aura-network when a push lands on orbit.
//!
//! After a successful receive-pack, orbit calls aura-network's POST /internal/posts
//! to create a feed post of type "push" with commit references and metadata.

use std::time::Duration;

use uuid::Uuid;

use crate::config::Config;
use crate::storage::git::GitCommand;

/// Parameters for creating a push post in the feed.
pub struct PushPostParams {
    pub repo_disk_path: std::path::PathBuf,
    pub repo_id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub actor_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub repo_name: String,
    pub branch: String,
}

/// Parse `git log --format=%H|%s` output into (sha, message) pairs.
fn parse_commit_log(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (sha, message) = l.split_once('|').unwrap_or((l, ""));
            (sha.to_string(), message.to_string())
        })
        .collect()
}

/// Build the JSON body for the aura-network push post.
fn build_push_post_body(
    params: &PushPostParams,
    commit_data: &[(String, String)],
) -> serde_json::Value {
    let commit_shas: Vec<&str> = commit_data.iter().map(|(sha, _)| sha.as_str()).collect();
    let commits_metadata: Vec<serde_json::Value> = commit_data
        .iter()
        .map(|(sha, msg)| serde_json::json!({"sha": sha, "message": msg}))
        .collect();

    let commit_count = commit_data.len();
    let title = format!(
        "Pushed {} commit{} to {}",
        commit_count,
        if commit_count == 1 { "" } else { "s" },
        params.repo_name
    );

    serde_json::json!({
        "profileId": params.actor_id,
        "orgId": params.org_id,
        "projectId": params.project_id,
        "eventType": "push",
        "postType": "push",
        "title": title,
        "userId": params.actor_id,
        "agentId": params.agent_id,
        "pushId": params.repo_id,
        "commitIds": commit_shas,
        "metadata": {
            "repo": params.repo_name,
            "branch": params.branch,
            "commits": commits_metadata,
        },
    })
}

/// Create a push post in aura-network's feed after a successful push.
///
/// This is called fire-and-forget after receive-pack completes.
/// Errors are logged but do not affect the push response.
pub async fn create_push_post(config: &Config, params: &PushPostParams) {
    let repo_disk_path = &params.repo_disk_path;
    let aura_network_url = match &config.aura_network_url {
        Some(url) => url,
        None => {
            tracing::debug!("AURA_NETWORK_URL not set, skipping push post");
            return;
        }
    };

    // Get recent commits from this push via git log on the bare repo.
    // Scoped to the pushed branch, includes commit messages.
    let git = GitCommand::new(repo_disk_path.to_path_buf());
    let ref_spec = format!("refs/heads/{}", params.branch);
    let commit_data = match git
        .run(&["log", "--format=%H|%s", "-n", "10", &ref_spec])
        .await
    {
        Ok(output) if output.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            parse_commit_log(&stdout)
        }
        _ => {
            tracing::warn!("Failed to get commit data for push post");
            vec![]
        }
    };

    let commit_count = commit_data.len();
    let body = build_push_post_body(params, &commit_data);

    // Post to aura-network feed with a timeout to prevent hanging indefinitely.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let url = format!("{}/internal/posts", aura_network_url);

    let response = client
        .post(&url)
        .header("X-Internal-Token", &config.internal_service_token)
        .json(&body)
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(
                repo_id = %params.repo_id,
                commits = commit_count,
                "Push post created in aura-network feed"
            );
        }
        Ok(resp) => {
            tracing::warn!(
                status = %resp.status(),
                "Failed to create push post in aura-network"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to call aura-network for push post");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commit_log_normal() {
        let output = "abc123def456789|feat: add feature\n0123456789abcde|fix: bug\n";
        let result = parse_commit_log(output);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "abc123def456789");
        assert_eq!(result[0].1, "feat: add feature");
        assert_eq!(result[1].0, "0123456789abcde");
        assert_eq!(result[1].1, "fix: bug");
    }

    #[test]
    fn parse_commit_log_message_with_pipe() {
        let output = "abc123|fix: handle a|b case\n";
        let result = parse_commit_log(output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "abc123");
        assert_eq!(result[0].1, "fix: handle a|b case");
    }

    #[test]
    fn parse_commit_log_empty() {
        let result = parse_commit_log("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_commit_log_sha_only_no_message() {
        let output = "abc123\n";
        let result = parse_commit_log(output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "abc123");
        assert_eq!(result[0].1, "");
    }

    #[test]
    fn build_push_post_body_includes_metadata() {
        let params = PushPostParams {
            repo_disk_path: std::path::PathBuf::from("/tmp/test"),
            repo_id: Uuid::nil(),
            org_id: Uuid::nil(),
            project_id: Uuid::nil(),
            actor_id: Uuid::nil(),
            agent_id: None,
            repo_name: "my-repo".to_string(),
            branch: "main".to_string(),
        };
        let commits = vec![("abc".to_string(), "feat: thing".to_string())];
        let body = build_push_post_body(&params, &commits);

        assert_eq!(body["metadata"]["repo"], "my-repo");
        assert_eq!(body["metadata"]["branch"], "main");
        assert_eq!(body["metadata"]["commits"][0]["sha"], "abc");
        assert_eq!(body["metadata"]["commits"][0]["message"], "feat: thing");
        assert_eq!(body["commitIds"][0], "abc");
        assert!(body["title"].as_str().unwrap().contains("1 commit"));
        assert!(body["title"].as_str().unwrap().contains("my-repo"));
        assert_eq!(body["postType"], "push");
        assert_eq!(body["eventType"], "push");
    }

    #[test]
    fn build_push_post_body_multiple_commits() {
        let params = PushPostParams {
            repo_disk_path: std::path::PathBuf::from("/tmp/test"),
            repo_id: Uuid::nil(),
            org_id: Uuid::nil(),
            project_id: Uuid::nil(),
            actor_id: Uuid::nil(),
            agent_id: Some(Uuid::nil()),
            repo_name: "backend".to_string(),
            branch: "develop".to_string(),
        };
        let commits = vec![
            ("sha1".to_string(), "first".to_string()),
            ("sha2".to_string(), "second".to_string()),
            ("sha3".to_string(), "third".to_string()),
        ];
        let body = build_push_post_body(&params, &commits);

        assert_eq!(body["metadata"]["branch"], "develop");
        assert_eq!(body["metadata"]["commits"].as_array().unwrap().len(), 3);
        assert_eq!(body["commitIds"].as_array().unwrap().len(), 3);
        assert!(body["title"].as_str().unwrap().contains("3 commits"));
        assert!(body["agentId"].is_string() || body["agentId"].is_null());
    }

    #[test]
    fn build_push_post_body_zero_commits() {
        let params = PushPostParams {
            repo_disk_path: std::path::PathBuf::from("/tmp/test"),
            repo_id: Uuid::nil(),
            org_id: Uuid::nil(),
            project_id: Uuid::nil(),
            actor_id: Uuid::nil(),
            agent_id: None,
            repo_name: "empty-repo".to_string(),
            branch: "main".to_string(),
        };
        let body = build_push_post_body(&params, &[]);

        assert_eq!(body["metadata"]["commits"], serde_json::json!([]));
        assert_eq!(body["commitIds"], serde_json::json!([]));
        assert!(body["title"].as_str().unwrap().contains("0 commits"));
    }
}

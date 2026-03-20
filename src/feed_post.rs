//! Feed post: auto-create a push post in aura-network when a push lands on orbit.
//!
//! After a successful receive-pack, orbit calls aura-network's POST /internal/activity
//! to create a feed post of type "push" with commit references.

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
}

/// Create a push post in aura-network's feed after a successful push.
///
/// This is called fire-and-forget after receive-pack completes.
/// Errors are logged but do not affect the push response.
pub async fn create_push_post(config: &Config, params: &PushPostParams) {
    let repo_disk_path = &params.repo_disk_path;
    let repo_id = params.repo_id;
    let org_id = params.org_id;
    let project_id = params.project_id;
    let actor_id = params.actor_id;
    let agent_id = params.agent_id;
    let repo_name = &params.repo_name;
    let aura_network_url = match &config.aura_network_url {
        Some(url) => url,
        None => {
            tracing::debug!("AURA_NETWORK_URL not set, skipping push post");
            return;
        }
    };

    // Get recent commits from this push via git log on the bare repo
    let git = GitCommand::new(repo_disk_path.to_path_buf());
    let commit_shas = match git.run(&["log", "--format=%H", "-n", "10", "HEAD"]).await {
        Ok(output) if output.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let shas: Vec<String> = stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            shas
        }
        _ => {
            tracing::warn!("Failed to get commit SHAs for push post");
            vec![]
        }
    };

    let commit_count = commit_shas.len();
    let title = format!(
        "Pushed {} commit{} to {}",
        commit_count,
        if commit_count == 1 { "" } else { "s" },
        repo_name
    );

    let commit_ids = serde_json::to_value(&commit_shas).unwrap_or_default();

    // Post to aura-network feed
    let client = reqwest::Client::new();
    let url = format!("{}/internal/activity", aura_network_url);

    let body = serde_json::json!({
        "profileId": actor_id,
        "orgId": org_id,
        "projectId": project_id,
        "eventType": "push",
        "postType": "push",
        "title": title,
        "userId": actor_id,
        "agentId": agent_id,
        "pushId": repo_id,
        "commitIds": commit_ids,
    });

    let response = client
        .post(&url)
        .header("X-Internal-Token", &config.internal_service_token)
        .json(&body)
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(
                repo_id = %repo_id,
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

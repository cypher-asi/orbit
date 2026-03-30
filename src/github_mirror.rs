//! GitHub mirror: push to a configured GitHub remote after receiving a push on orbit.
//!
//! When an org has a GitHub integration configured in aura-network, orbit
//! mirrors pushes to the GitHub repo as a secondary/backup.

use std::path::Path;

use serde::Deserialize;
use uuid::Uuid;

use crate::config::Config;

/// GitHub integration config from aura-network's org_integrations table.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntegrationResponse {
    integration_type: String,
    config: GitHubConfig,
    enabled: bool,
}

/// The JSONB config stored in aura-network for a GitHub integration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubConfig {
    /// GitHub repository owner (user or org).
    owner: String,
    /// GitHub repository name.
    repo: String,
    /// GitHub personal access token for pushing.
    #[serde(default)]
    token: Option<String>,
}

/// Check if the org has a GitHub integration and mirror the push if configured.
///
/// This is called fire-and-forget after a successful receive-pack.
/// Uses X-Internal-Token to query aura-network for integration config
/// (service-to-service, per production architecture doc).
/// Errors are logged but do not affect the push response.
pub async fn mirror_if_configured(config: &Config, org_id: Uuid, repo_disk_path: &Path) {
    let aura_network_url = match &config.aura_network_url {
        Some(url) => url,
        None => {
            tracing::debug!("AURA_NETWORK_URL not set, skipping GitHub mirror check");
            return;
        }
    };

    // Query aura-network for integrations on this org (service-to-service)
    let client = reqwest::Client::new();
    let url = format!("{}/internal/orgs/{}/integrations", aura_network_url, org_id);

    let response = match client
        .get(&url)
        .header("X-Internal-Token", &config.internal_service_token)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, "GitHub mirror: failed to query aura-network integrations");
            return;
        }
    };

    if !response.status().is_success() {
        tracing::warn!(
            status = %response.status(),
            "GitHub mirror: aura-network integrations query failed"
        );
        return;
    }

    let integrations: Vec<IntegrationResponse> = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "GitHub mirror: failed to parse integrations response");
            return;
        }
    };

    // Find a GitHub integration that is enabled
    let github = integrations
        .into_iter()
        .find(|i| i.integration_type == "github" && i.enabled);

    let github = match github {
        Some(g) => g,
        None => {
            tracing::debug!(org_id = %org_id, "No enabled GitHub integration for org");
            return;
        }
    };

    let token = match &github.config.token {
        Some(t) if !t.is_empty() => t,
        _ => {
            tracing::warn!(
                org_id = %org_id,
                "GitHub integration configured but no token set, skipping mirror"
            );
            return;
        }
    };

    // Build the GitHub remote URL with token auth
    let remote_url = format!(
        "https://x-access-token:{}@github.com/{}/{}.git",
        token, github.config.owner, github.config.repo
    );

    // Push all refs to the GitHub remote
    tracing::info!(
        org_id = %org_id,
        github_repo = %format!("{}/{}", github.config.owner, github.config.repo),
        "Mirroring push to GitHub"
    );

    let output = tokio::process::Command::new("git")
        .args(["push", "--mirror", &remote_url])
        .env("GIT_DIR", repo_disk_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            tracing::info!(
                org_id = %org_id,
                github_repo = %format!("{}/{}", github.config.owner, github.config.repo),
                "GitHub mirror push successful"
            );
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let sanitized = stderr.replace(token, "***");
            tracing::error!(
                org_id = %org_id,
                stderr = %sanitized,
                "GitHub mirror push failed"
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to spawn git push for GitHub mirror");
        }
    }
}

//! Discovery endpoint for API and Git URL configuration.
//!
//! GET / and GET /api return JSON describing the server's API version,
//! base URL, and Git clone URL prefix so clients (e.g. aura-app) can
//! configure the orbit server without hardcoding paths.

use axum::{extract::State, Json};
use serde::Serialize;

use crate::app_state::AppState;

/// Discovery response body.
#[derive(Debug, Serialize)]
pub(crate) struct DiscoveryResponse {
    /// API version (e.g. "1" for /v1).
    pub api_version: String,
    /// Base URL for the REST API (no trailing slash).
    pub base_url: String,
    /// Prefix for Git clone URLs: clone = `{git_url_prefix}{owner}/{repo}.git`.
    pub git_url_prefix: String,
    /// Auth scheme: "bearer" (Bearer token in Authorization header).
    pub auth: String,
}

/// GET / or GET /api — Discovery endpoint.
///
/// Returns server metadata so clients can configure the orbit base URL,
/// API version, and Git remote. No authentication required.
pub(crate) async fn discovery(State(state): State<AppState>) -> Json<DiscoveryResponse> {
    let base_url = state.config.base_url();
    let git_url_prefix = state.config.git_url_prefix();

    Json(DiscoveryResponse {
        api_version: "1".to_string(),
        base_url,
        git_url_prefix,
        auth: "bearer".to_string(),
    })
}

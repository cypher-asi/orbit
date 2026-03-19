use axum::http::header::HeaderName;
use axum::{routing::get, Router};
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::api::rate_limit::RateLimitError;
use crate::app_state::AppState;
use crate::config::Config;

use anyhow::Context;

use super::discovery::discovery;
use super::health::health_check;
use super::rate_limit::{
    admin_action_rate_limit_layer, admin_action_rate_limit_layer_redis, auth_rate_limit_layer,
    auth_rate_limit_layer_redis, git_receive_rate_limit_layer, git_receive_rate_limit_layer_redis,
    repo_create_rate_limit_layer, repo_create_rate_limit_layer_redis, repo_write_rate_limit_layer,
    repo_write_rate_limit_layer_redis, token_rate_limit_layer, token_rate_limit_layer_redis,
    RateLimitLayer,
};

// ---------------------------------------------------------------------------
// Pre-built rate limit layers
// ---------------------------------------------------------------------------

/// Holds all pre-built rate limit layers for use across route groups.
///
/// Layers are built once at startup -- either backed by Redis (when
/// `config.redis_url` is set) or by the in-memory governor backend.
/// This avoids making individual route-group functions async and keeps
/// layer construction centralized.
struct RateLimitLayers {
    /// Rate limit layer for auth login/register endpoints (10 req/min per IP).
    auth: RateLimitLayer,
    /// Rate limit layer for token creation endpoints (20 req/min per IP).
    token: RateLimitLayer,
    /// Rate limit layer for repo creation (30 req/min per IP).
    repo_create: RateLimitLayer,
    /// Rate limit layer for write-heavy repo operations (30 req/min per IP).
    repo_write: RateLimitLayer,
    /// Rate limit layer for admin mutation endpoints (30 req/min per IP).
    admin_action: RateLimitLayer,
    /// Rate limit layer for git push (receive-pack) operations (30 req/min per IP).
    git_receive: RateLimitLayer,
}

impl RateLimitLayers {
    /// Build all rate limit layers, using Redis when `redis_url` is configured
    /// and falling back to in-memory when it is not (or when Redis connection
    /// fails).
    async fn build(config: &Config) -> Result<Self, RateLimitError> {
        match config.redis_url.as_deref() {
            Some(redis_url) => {
                tracing::info!("Building Redis-backed rate limit layers");
                Ok(Self {
                    auth: auth_rate_limit_layer_redis(redis_url).await?,
                    token: token_rate_limit_layer_redis(redis_url).await?,
                    repo_create: repo_create_rate_limit_layer_redis(redis_url).await?,
                    repo_write: repo_write_rate_limit_layer_redis(redis_url).await?,
                    admin_action: admin_action_rate_limit_layer_redis(redis_url).await?,
                    git_receive: git_receive_rate_limit_layer_redis(redis_url).await?,
                })
            }
            None => {
                tracing::info!("Building in-memory rate limit layers (no REDIS_URL configured)");
                Ok(Self {
                    auth: auth_rate_limit_layer()?,
                    token: token_rate_limit_layer()?,
                    repo_create: repo_create_rate_limit_layer()?,
                    repo_write: repo_write_rate_limit_layer()?,
                    admin_action: admin_action_rate_limit_layer()?,
                    git_receive: git_receive_rate_limit_layer()?,
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Route group builders
// ---------------------------------------------------------------------------

/// User profile routes.
///
/// With zOS JWT auth, login/register/PAT endpoints are removed.
/// User identity comes from the JWT token.
fn auth_routes(_layers: &RateLimitLayers) -> Router<AppState> {
    Router::new()
}

/// Aggregate all repository-related routes under `/repos/...`.
///
/// Merges the core repo CRUD routes together with sub-resource routes for
/// branches, commits, pull requests, merge operations, collaborators, and
/// repo-scoped events.
///
/// Rate limiting is applied to:
/// - `POST /repos` (repo creation) -- 30/min per IP
/// - `POST /repos/{org_id}/{repo}/pulls` and merge -- 30/min per IP
fn repos_routes(layers: &RateLimitLayers) -> Router<AppState> {
    use axum::routing::post;

    // Rate-limited repo creation (30/min per IP)
    // Prevents mass creation of repositories by a single IP.
    let rate_limited_repo_create = Router::new()
        .route("/repos", post(crate::repos::routes::create_repo_handler()))
        .layer(layers.repo_create.clone());

    // Rate-limited write-heavy operations: PR creation and merge (30/min per IP)
    // These are computationally expensive and should be protected.
    let rate_limited_repo_writes = Router::new()
        .route(
            "/repos/{org_id}/{repo}/pulls",
            post(crate::pull_requests::routes::create_pr_handler()),
        )
        .route(
            "/repos/{org_id}/{repo}/pulls/{id}/merge",
            post(crate::merge_engine::routes::merge_pr_handler()),
        )
        .layer(layers.repo_write.clone());

    Router::new()
        // Rate-limited repo creation
        .merge(rate_limited_repo_create)
        // Rate-limited PR creation and merge
        .merge(rate_limited_repo_writes)
        // Core repo CRUD (non-rate-limited parts): GET /repos, GET/PATCH/DELETE /repos/{org_id}/{repo}, etc.
        .merge(crate::repos::routes::repo_routes_without_create())
        // Collaborators: GET/PUT/DELETE /repos/{org_id}/{repo}/collaborators/...
        .merge(crate::permissions::routes::collaborator_routes())
        // Branches: GET/POST/DELETE /repos/{org_id}/{repo}/branches/...
        .merge(crate::branches::routes::branches_routes())
        // Commits & tree browsing: GET /repos/{org_id}/{repo}/commits/...
        .merge(crate::commits::routes::commits_routes())
        // Tags: GET /repos/{org_id}/{repo}/tags
        .merge(crate::tags::routes::tags_routes())
        // Pull requests (non-rate-limited parts): GET/PATCH, close, reopen, diff, mergeability
        .merge(crate::pull_requests::routes::pull_request_routes_without_create())
        // Merge engine (non-rate-limited parts -- the merge POST is above)
        // Note: merge_engine_routes_without_merge() is empty since there is only one route
        // Repo-scoped events: GET /repos/{org_id}/{repo}/events
        .merge(crate::events::routes::repo_event_routes())
}

/// Placeholder for user-related routes.
///
/// With zOS JWT auth, user management is handled by aura-network.
/// This empty router exists to maintain the route group structure.
fn users_routes() -> Router<AppState> {
    Router::new()
}

/// Aggregate all admin routes.
///
/// Includes:
/// - Admin user management (`/admin/users/...`)
/// - Admin repo management (`/admin/repos/...`)
/// - Admin job management  (`/admin/jobs/...`)
/// - Admin audit events    (`/admin/events/...`)
///
/// Rate limiting (30/min per IP) is applied to admin mutation endpoints
/// (disable/enable users, archive repos, retry jobs) for defense-in-depth.
fn admin_routes(layers: &RateLimitLayers) -> Router<AppState> {
    // Rate-limited admin mutation actions (30/min per IP)
    let rate_limited_admin_actions = Router::new()
        .merge(crate::admin::routes::admin_mutation_routes())
        .layer(layers.admin_action.clone());

    Router::new()
        // Rate-limited admin mutations
        .merge(rate_limited_admin_actions)
        // Non-rate-limited admin read routes and admin event routes
        .merge(crate::admin::routes::admin_read_routes())
        // Admin event routes
        .merge(crate::events::routes::admin_event_routes())
}

/// Git HTTP transport routes.
///
/// Includes:
/// - `GET  /{org_id}/{repo}/info/refs`
/// - `POST /{org_id}/{repo}/git-upload-pack`
/// - `POST /{org_id}/{repo}/git-receive-pack`
///
/// These use `{repo}` that includes the `.git` suffix (e.g. `my-repo.git`),
/// so they naturally do not conflict with API routes under `/repos/...`.
///
/// Rate limiting (30/min per IP) is applied to `git-receive-pack` (push)
/// since push operations are expensive (disk I/O, pack processing).
fn git_http_routes(layers: &RateLimitLayers) -> Router<AppState> {
    // Rate-limited push endpoint (30/min per IP)
    let rate_limited_push = Router::new()
        .merge(crate::git_http::routes::git_receive_routes())
        .layer(layers.git_receive.clone());

    Router::new()
        // Rate-limited push
        .merge(rate_limited_push)
        // Non-rate-limited read routes (info/refs and upload-pack)
        .merge(crate::git_http::routes::git_read_routes())
}

// ---------------------------------------------------------------------------
// Middleware stack
// ---------------------------------------------------------------------------

/// Header name used for request IDs.
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Build the CORS layer based on configuration.
///
/// When `config.cors_allowed_origins` is empty, any origin is allowed
/// (suitable for development). When one or more origins are specified,
/// only those origins are permitted (suitable for production).
fn build_cors_layer(config: &Config, request_id_header: &HeaderName) -> anyhow::Result<CorsLayer> {
    let base = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers([request_id_header.clone()]);

    if config.cors_allowed_origins.is_empty() {
        Ok(base.allow_origin(Any))
    } else {
        let origins: Vec<axum::http::HeaderValue> = config
            .cors_allowed_origins
            .iter()
            .map(|o| {
                o.parse()
                    .with_context(|| format!("Invalid CORS origin: {}", o))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(base.allow_origin(origins))
    }
}

/// Build the layered middleware stack applied to all routes.
///
/// Order (outermost to innermost):
/// 1. **Set Request ID** -- generates a UUID v4 for each request
/// 2. **Tracing** -- logs method, path, status, and duration
/// 3. **CORS** -- configurable allowed origins (permissive when unset)
/// 4. **Propagate Request ID** -- copies request ID to response headers
fn apply_middleware(
    router: Router<AppState>,
    config: &Config,
    request_id_header: &HeaderName,
) -> anyhow::Result<Router<AppState>> {
    let cors = build_cors_layer(config, request_id_header)?;

    let request_id_header = request_id_header.clone();
    let request_id_header_for_trace = request_id_header.clone();
    Ok(router
        // Propagate x-request-id from request to response
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        // CORS headers
        .layer(cors)
        // Request tracing (logs method, URI, status, latency)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(move |request: &axum::http::Request<_>| {
                    let request_id = request
                        .headers()
                        .get(&request_id_header_for_trace)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");
                    tracing::info_span!(
                        "request",
                        method = %request.method(),
                        uri = %request.uri(),
                        request_id = %request_id,
                    )
                })
                .on_response(
                    |response: &axum::http::Response<_>,
                     latency: std::time::Duration,
                     _span: &tracing::Span| {
                        tracing::info!(
                            status = %response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "response"
                        );
                    },
                ),
        )
        // Set x-request-id (UUID v4) on incoming requests
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid)))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the complete application router with all module routes and middleware.
///
/// ## Route Groups
///
/// | Prefix | Source |
/// |--------|--------|
/// | `/health` | Health check |
/// | `/repos/*` | Repositories, branches, commits, PRs, merge, collaborators |
/// | `/admin/*` | Admin management (repos, jobs, events) |
/// | `/internal/*` | Service-to-service endpoints (X-Internal-Token auth) |
/// | `/{org_id}/{repo}.git/*` | Git HTTP transport (info/refs, upload-pack, receive-pack) |
///
/// ## Middleware Stack
///
/// Applied to every request:
/// 1. Request ID generation (`x-request-id` header)
/// 2. Request tracing (method, URI, status, latency)
/// 3. CORS (configurable allowed origins; permissive when `CORS_ORIGINS` is unset)
/// 4. Request ID propagation to response
///
/// ## Rate Limiting
///
/// Rate limit layers are built once at startup. When `config.redis_url` is set,
/// all rate limiters use Redis as a shared backend for consistent rate limiting
/// across multiple server instances. Otherwise, in-memory governor-based rate
/// limiters are used.
pub async fn build_router(state: AppState) -> anyhow::Result<Router> {
    let request_id_header = REQUEST_ID_HEADER
        .parse::<HeaderName>()
        .context("invalid x-request-id header name")?;

    // Build rate limit layers at startup, using Redis when configured.
    let layers = RateLimitLayers::build(&state.config)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))
        .context("build rate limit layers")?;

    // Versioned API under /v1 (same REST routes as root, plus discovery at GET /v1 and GET /v1/api).
    let v1_router = Router::new()
        .route("/", get(discovery))
        .route("/api", get(discovery))
        .merge(auth_routes(&layers))
        .merge(repos_routes(&layers))
        .merge(users_routes())
        .merge(admin_routes(&layers));

    let app = Router::new()
        // Health check at the root level
        .route("/health", get(health_check))
        // Discovery (no auth): GET / and GET /api
        .route("/", get(discovery))
        .route("/api", get(discovery))
        // Auth routes (placeholder — auth is JWT-based, no user-facing auth endpoints)
        .merge(auth_routes(&layers))
        // Repository routes (CRUD, branches, commits, PRs, merge, collaborators, events)
        .merge(repos_routes(&layers))
        // User routes (placeholder — user management is in aura-network)
        .merge(users_routes())
        // Admin routes (repo/job management, audit events)
        .merge(admin_routes(&layers))
        // Versioned API under /v1
        .nest("/v1", v1_router)
        // Internal endpoints (X-Internal-Token auth, service-to-service)
        .merge(crate::internal::internal_routes())
        // Git HTTP transport -- mounted at root; paths use `{repo}.git` suffix
        // so they don't conflict with `/repos/{org_id}/{repo}/...` API routes
        .merge(git_http_routes(&layers));

    // Apply the middleware stack and attach shared state
    let app = apply_middleware(app, &state.config, &request_id_header)?;
    Ok(app.with_state(state))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `build_router` compiles and produces a valid `Router`.
    /// This is a compile-time + basic runtime sanity check; actual route
    /// behavior is tested in each module's own test suite.
    #[test]
    fn request_id_header_is_valid() {
        // Ensure the header name parses without panic.
        let _: axum::http::HeaderName = REQUEST_ID_HEADER.parse().unwrap();
    }

    #[test]
    fn cors_layer_builds_permissive() {
        // When no origins are configured, allow any origin (development mode).
        let config = Config {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 3000,
            git_storage_root: String::new(),
            log_level: String::new(),
            cors_allowed_origins: vec![],
            redis_url: None,
            public_base_url: None,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth_cookie_secret: String::new(),
            internal_service_token: String::new(),
        };
        let header = REQUEST_ID_HEADER.parse().unwrap();
        let _cors = build_cors_layer(&config, &header).unwrap();
    }

    #[test]
    fn cors_layer_builds_with_allowed_origins() {
        // When specific origins are configured, build a restrictive layer.
        let config = Config {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 3000,
            git_storage_root: String::new(),
            log_level: String::new(),
            cors_allowed_origins: vec![
                "https://example.com".to_string(),
                "https://app.example.com".to_string(),
            ],
            redis_url: None,
            public_base_url: None,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth_cookie_secret: String::new(),
            internal_service_token: String::new(),
        };
        let header = REQUEST_ID_HEADER.parse().unwrap();
        let _cors = build_cors_layer(&config, &header).unwrap();
    }

    #[test]
    fn cors_layer_errors_on_invalid_origin() {
        let config = Config {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 3000,
            git_storage_root: String::new(),
            log_level: String::new(),
            cors_allowed_origins: vec!["not a valid\norigin".to_string()],
            redis_url: None,
            public_base_url: None,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth_cookie_secret: String::new(),
            internal_service_token: String::new(),
        };
        let header = REQUEST_ID_HEADER.parse().unwrap();
        assert!(build_cors_layer(&config, &header).is_err());
    }

    #[tokio::test]
    async fn rate_limit_layers_build_in_memory_when_no_redis() {
        // When redis_url is None, all layers should be built with in-memory
        // backends (no Redis connection attempted).
        let config = Config {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 3000,
            git_storage_root: String::new(),
            log_level: String::new(),
            cors_allowed_origins: vec![],
            redis_url: None,
            public_base_url: None,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth_cookie_secret: String::new(),
            internal_service_token: String::new(),
        };
        let layers = RateLimitLayers::build(&config)
            .await
            .expect("in-memory rate limit config is valid");
        // Verify all layer fields are populated (they are -- this is a
        // compile-time + construction check).
        let _ = layers.auth.clone();
        let _ = layers.token.clone();
        let _ = layers.repo_create.clone();
        let _ = layers.repo_write.clone();
        let _ = layers.admin_action.clone();
        let _ = layers.git_receive.clone();
    }
}

use axum::{routing::get, Router};
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::app_state::AppState;
use crate::config::Config;

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
    async fn build(config: &Config) -> Self {
        match config.redis_url.as_deref() {
            Some(redis_url) => {
                tracing::info!("Building Redis-backed rate limit layers");
                Self {
                    auth: auth_rate_limit_layer_redis(redis_url).await,
                    token: token_rate_limit_layer_redis(redis_url).await,
                    repo_create: repo_create_rate_limit_layer_redis(redis_url).await,
                    repo_write: repo_write_rate_limit_layer_redis(redis_url).await,
                    admin_action: admin_action_rate_limit_layer_redis(redis_url).await,
                    git_receive: git_receive_rate_limit_layer_redis(redis_url).await,
                }
            }
            None => {
                tracing::info!("Building in-memory rate limit layers (no REDIS_URL configured)");
                Self {
                    auth: auth_rate_limit_layer(),
                    token: token_rate_limit_layer(),
                    repo_create: repo_create_rate_limit_layer(),
                    repo_write: repo_write_rate_limit_layer(),
                    admin_action: admin_action_rate_limit_layer(),
                    git_receive: git_receive_rate_limit_layer(),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Route group builders
// ---------------------------------------------------------------------------

/// Aggregate all auth-related routes.
///
/// Includes:
/// - `POST /auth/register` (from users::routes) -- rate-limited (10/min per IP)
/// - `POST /auth/login`    (from auth::routes)  -- rate-limited (10/min per IP)
/// - `POST /auth/tokens`   (from auth::routes)  -- rate-limited (20/min per IP)
/// - `GET  /auth/tokens`   (from auth::routes)
/// - `DELETE /auth/tokens/{id}` (from auth::routes)
/// - `GET  /users/me`      (from users::routes)
/// - `PATCH /users/me`     (from users::routes)
///
/// Rate limiting is applied to `/auth/login` and `/auth/register` to prevent
/// brute-force attacks and credential stuffing. Token creation is rate-limited
/// at a higher threshold (20/min) since it requires authentication but should
/// still be protected against abuse.
///
/// When `config.redis_url` is set, the rate limit layers use Redis as a shared
/// backend for consistent rate limiting across multiple server instances.
fn auth_routes(layers: &RateLimitLayers) -> Router<AppState> {
    use crate::users::routes::register;
    use axum::routing::post;

    // Rate-limited routes: login and register (10/min per IP)
    // These are the most sensitive endpoints for brute-force protection.
    // Login is handled by auth::routes which supports username or email,
    // delegates to the auth service layer, and uses the centralized token module.
    let rate_limited_auth = Router::new()
        .route("/auth/register", post(register))
        .merge(crate::auth::routes::auth_login_routes())
        .layer(layers.auth.clone());

    // Rate-limited token creation (20/min per IP)
    // Token creation requires authentication, so brute-force is less of a
    // concern, but we still rate-limit to prevent abuse.
    let rate_limited_tokens = Router::new()
        .route(
            "/auth/tokens",
            post(crate::auth::routes::create_token_handler_fn()),
        )
        .layer(layers.token.clone());

    // Non-rate-limited auth routes (token listing and revocation)
    // These require authentication and are read/delete operations.
    let other_auth = Router::new().merge(crate::auth::routes::auth_token_read_routes());

    // User profile routes (GET/PATCH /users/me)
    let user_profile = Router::new().merge(crate::users::routes::users_profile_routes());

    Router::new()
        .merge(rate_limited_auth)
        .merge(rate_limited_tokens)
        .merge(other_auth)
        .merge(user_profile)
}

/// Aggregate all repository-related routes under `/repos/...`.
///
/// Merges the core repo CRUD routes together with sub-resource routes for
/// branches, commits, pull requests, merge operations, collaborators, and
/// repo-scoped events.
///
/// Rate limiting is applied to:
/// - `POST /repos` (repo creation) -- 30/min per IP
/// - `POST /repos/{owner}/{repo}/pulls` and merge -- 30/min per IP
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
            "/repos/{owner}/{repo}/pulls",
            post(crate::pull_requests::routes::create_pr_handler()),
        )
        .route(
            "/repos/{owner}/{repo}/pulls/{id}/merge",
            post(crate::merge_engine::routes::merge_pr_handler()),
        )
        .layer(layers.repo_write.clone());

    Router::new()
        // Rate-limited repo creation
        .merge(rate_limited_repo_create)
        // Rate-limited PR creation and merge
        .merge(rate_limited_repo_writes)
        // Core repo CRUD (non-rate-limited parts): GET /repos, GET/PATCH/DELETE /repos/{owner}/{repo}, etc.
        .merge(crate::repos::routes::repo_routes_without_create())
        // Collaborators: GET/PUT/DELETE /repos/{owner}/{repo}/collaborators/...
        .merge(crate::permissions::routes::collaborator_routes())
        // Branches: GET/POST/DELETE /repos/{owner}/{repo}/branches/...
        .merge(crate::branches::routes::branches_routes())
        // Commits & tree browsing: GET /repos/{owner}/{repo}/commits/...
        .merge(crate::commits::routes::commits_routes())
        // Tags: GET /repos/{owner}/{repo}/tags
        .merge(crate::tags::routes::tags_routes())
        // Pull requests (non-rate-limited parts): GET/PATCH, close, reopen, diff, mergeability
        .merge(crate::pull_requests::routes::pull_request_routes_without_create())
        // Merge engine (non-rate-limited parts -- the merge POST is above)
        // Note: merge_engine_routes_without_merge() is empty since there is only one route
        // Repo-scoped events: GET /repos/{owner}/{repo}/events
        .merge(crate::events::routes::repo_event_routes())
}

/// Aggregate all user-profile routes.
///
/// Includes:
/// - `GET  /users/me`
/// - `PATCH /users/me`
/// - `GET  /users/{username}/repos` (served from repos::routes)
///
/// Note: user profile routes (`/users/me`) are part of `users_routes()` which
/// is already merged via `auth_routes()`. The `/users/{username}/repos` route
/// is part of `repo_routes()`. This function exists as a logical grouping
/// point; in practice, the routes are contributed by those merged routers.
fn users_routes() -> Router<AppState> {
    // The /users/me endpoints come from users::routes::users_routes() which
    // is already merged in auth_routes(). The /users/{username}/repos route
    // lives in repos::routes::repo_routes() which is merged in repos_routes().
    // Return an empty router here -- the routes are already covered.
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
/// - `GET  /{owner}/{repo}/info/refs`
/// - `POST /{owner}/{repo}/git-upload-pack`
/// - `POST /{owner}/{repo}/git-receive-pack`
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
fn build_cors_layer(config: &Config) -> CorsLayer {
    let base = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers([REQUEST_ID_HEADER.parse().unwrap()]);

    if config.cors_allowed_origins.is_empty() {
        // Development: allow all origins
        base.allow_origin(Any)
    } else {
        // Production: restrict to configured origins
        let origins: Vec<axum::http::HeaderValue> = config
            .cors_allowed_origins
            .iter()
            .map(|o| {
                o.parse()
                    .unwrap_or_else(|_| panic!("Invalid CORS origin: {}", o))
            })
            .collect();
        base.allow_origin(origins)
    }
}

/// Build the layered middleware stack applied to all routes.
///
/// Order (outermost to innermost):
/// 1. **Set Request ID** -- generates a UUID v4 for each request
/// 2. **Tracing** -- logs method, path, status, and duration
/// 3. **CORS** -- configurable allowed origins (permissive when unset)
/// 4. **Propagate Request ID** -- copies request ID to response headers
fn apply_middleware(router: Router<AppState>, config: &Config) -> Router<AppState> {
    let cors = build_cors_layer(config);

    let x_request_id = REQUEST_ID_HEADER.parse().unwrap();

    router
        // Propagate x-request-id from request to response
        .layer(PropagateRequestIdLayer::new(
            REQUEST_ID_HEADER.parse().unwrap(),
        ))
        // CORS headers
        .layer(cors)
        // Request tracing (logs method, URI, status, latency)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    let request_id = request
                        .headers()
                        .get(REQUEST_ID_HEADER)
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
        .layer(SetRequestIdLayer::new(x_request_id, MakeRequestUuid))
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
/// | `/auth/*` | Auth (login, register, PAT management) |
/// | `/repos/*` | Repositories, branches, commits, PRs, merge, collaborators |
/// | `/users/*` | User profiles |
/// | `/admin/*` | Admin management (users, repos, jobs, events) |
/// | `/{owner}/{repo}.git/*` | Git HTTP transport (info/refs, upload-pack, receive-pack) |
///
/// ## Middleware Stack
///
/// Applied to every request:
/// 1. Request ID generation (`x-request-id` header)
/// 2. Request tracing (method, URI, status, latency)
/// 3. CORS (configurable allowed origins; permissive when `CORS_ALLOWED_ORIGINS` is unset)
/// 4. Request ID propagation to response
///
/// ## Rate Limiting
///
/// Rate limit layers are built once at startup. When `config.redis_url` is set,
/// all rate limiters use Redis as a shared backend for consistent rate limiting
/// across multiple server instances. Otherwise, in-memory governor-based rate
/// limiters are used.
pub async fn build_router(state: AppState) -> Router {
    // Build rate limit layers at startup, using Redis when configured.
    let layers = RateLimitLayers::build(&state.config).await;

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
        // Auth routes (register, login, PAT CRUD, /users/me)
        .merge(auth_routes(&layers))
        // Repository routes (CRUD, branches, commits, PRs, merge, collaborators, events)
        .merge(repos_routes(&layers))
        // User routes (logical group -- actual routes merged via auth_routes / repos_routes)
        .merge(users_routes())
        // Admin routes (user/repo/job management, audit events)
        .merge(admin_routes(&layers))
        // Versioned API: same REST under /v1 (e.g. /v1/repos, /v1/auth/login)
        .nest("/v1", v1_router)
        // Git HTTP transport -- mounted at root; paths use `{repo}.git` suffix
        // so they don't conflict with `/repos/{owner}/{repo}/...` API routes
        .merge(git_http_routes(&layers));

    // Apply the middleware stack and attach shared state
    apply_middleware(app, &state.config).with_state(state)
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
        };
        let _cors = build_cors_layer(&config);
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
        };
        let _cors = build_cors_layer(&config);
    }

    #[test]
    #[should_panic(expected = "Invalid CORS origin")]
    fn cors_layer_panics_on_invalid_origin() {
        let config = Config {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 3000,
            git_storage_root: String::new(),
            log_level: String::new(),
            cors_allowed_origins: vec!["not a valid\norigin".to_string()],
            redis_url: None,
            public_base_url: None,
        };
        let _cors = build_cors_layer(&config);
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
        };
        let layers = RateLimitLayers::build(&config).await;
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

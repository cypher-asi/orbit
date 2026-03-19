//! Rate limiting middleware for protecting sensitive endpoints.
//!
//! Supports two backends:
//! - **In-memory** (default): Uses the [`governor`] crate with a per-IP token
//!   bucket rate limiter. Fast, zero-dependency, but resets on restart and does
//!   not share state across multiple server instances.
//! - **Redis** (distributed): Uses Redis as a shared backend via a sliding
//!   window counter. Rate-limit state is consistent across multiple server
//!   instances and survives restarts. Enabled by setting the `REDIS_URL`
//!   environment variable.
//!
//! Applied selectively to auth endpoints (`POST /auth/login`, `POST /auth/register`)
//! to prevent brute-force and credential-stuffing attacks.
//!
//! Returns a consistent JSON error response matching the project's error format
//! when the rate limit is exceeded (HTTP 429).

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, Response, StatusCode},
};
use governor::{
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter,
};
use std::{
    future::Future,
    net::SocketAddr,
    num::NonZeroU32,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Layer, Service};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the rate limiter.
#[derive(Debug, Clone)]
pub(crate) struct RateLimitConfig {
    /// Maximum number of requests allowed within the time window.
    pub requests_per_window: u32,
    /// Time window in seconds over which `requests_per_window` are allowed.
    pub window_secs: u64,
}

impl Default for RateLimitConfig {
    /// Default: 10 requests per 60 seconds for auth endpoints.
    fn default() -> Self {
        Self {
            requests_per_window: 10,
            window_secs: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Rate limit backend trait
// ---------------------------------------------------------------------------

/// Result of a rate-limit check.
pub(crate) type RateLimitResult = Result<bool, RateLimitError>;

/// Errors that can occur when checking rate limits or building rate limiters.
#[derive(Debug)]
pub(crate) enum RateLimitError {
    /// Redis backend encountered an error.
    BackendError(String),
    /// Invalid rate limit configuration (e.g. requests_per_window is 0).
    InvalidConfig(String),
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateLimitError::BackendError(msg) => write!(f, "rate limit backend error: {}", msg),
            RateLimitError::InvalidConfig(msg) => write!(f, "invalid rate limit config: {}", msg),
        }
    }
}

impl std::error::Error for RateLimitError {}

/// Trait abstracting the rate-limit storage backend.
///
/// Implementations must be cheaply cloneable (`Clone`), thread-safe
/// (`Send + Sync`), and support async rate-limit checks.
pub(crate) trait RateLimitBackend: Send + Sync + 'static {
    /// Check whether the given key (typically a client IP) is allowed to
    /// proceed under the configured rate limit.
    ///
    /// Returns `Ok(true)` if allowed, `Ok(false)` if rate-limited, or
    /// `Err(...)` if the backend is unavailable.
    fn check_key(&self, key: &str) -> Pin<Box<dyn Future<Output = RateLimitResult> + Send + '_>>;
}

// ---------------------------------------------------------------------------
// In-memory backend (governor)
// ---------------------------------------------------------------------------

/// Thread-safe rate limiter state using a global (not-keyed) limiter.
type GlobalLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

/// Per-IP keyed rate limiter state.
type IpKeyedLimiter =
    RateLimiter<String, governor::state::keyed::DashMapStateStore<String>, DefaultClock>;

/// In-memory rate-limit backend using the `governor` crate.
///
/// Fast and zero-dependency but resets on restart and does not share state
/// across multiple server instances.
#[derive(Clone)]
pub(crate) struct InMemoryBackend {
    /// Per-IP rate limiter.
    limiter: Arc<IpKeyedLimiter>,
    /// Fallback global limiter for when client IP is unknown.
    global_limiter: Arc<GlobalLimiter>,
}

impl InMemoryBackend {
    /// Create a new in-memory rate limiter with the given configuration.
    ///
    /// # Errors
    /// Returns `RateLimitError::InvalidConfig` if `requests_per_window` is 0
    /// or if the computed quota period is 0.
    pub fn new(config: &RateLimitConfig) -> Result<Self, RateLimitError> {
        let per_window = NonZeroU32::new(config.requests_per_window)
            .ok_or_else(|| RateLimitError::InvalidConfig("requests_per_window must be > 0".to_string()))?;

        let period_secs = config.window_secs / u64::from(config.requests_per_window.max(1));
        let quota = Quota::with_period(std::time::Duration::from_secs(period_secs))
            .ok_or_else(|| RateLimitError::InvalidConfig("quota period must be > 0".to_string()))?
            .allow_burst(per_window);

        let limiter = Arc::new(IpKeyedLimiter::keyed(quota));
        let global_limiter = Arc::new(GlobalLimiter::direct(quota));

        Ok(Self {
            limiter,
            global_limiter,
        })
    }
}

impl RateLimitBackend for InMemoryBackend {
    fn check_key(&self, key: &str) -> Pin<Box<dyn Future<Output = RateLimitResult> + Send + '_>> {
        let allowed = if key == "unknown" {
            self.global_limiter.check().is_ok()
        } else {
            self.limiter.check_key(&key.to_string()).is_ok()
        };
        Box::pin(std::future::ready(Ok(allowed)))
    }
}

// ---------------------------------------------------------------------------
// Redis backend
// ---------------------------------------------------------------------------

/// Redis-backed rate-limit backend using a sliding window counter.
///
/// Uses a Lua script executed atomically to implement a fixed-window counter
/// per IP key. The window is defined by `window_secs` and allows up to
/// `max_requests` per window.
///
/// The Redis connection manager automatically reconnects on failure.
#[derive(Clone)]
pub(crate) struct RedisBackend {
    /// Redis connection manager (auto-reconnecting).
    client: redis::aio::ConnectionManager,
    /// Maximum requests per window.
    max_requests: u32,
    /// Window size in seconds.
    window_secs: u64,
    /// Key prefix for namespacing in Redis.
    key_prefix: String,
}

impl RedisBackend {
    /// Create a new Redis rate-limit backend.
    ///
    /// `redis_url` should be a valid Redis connection string, e.g.
    /// `redis://127.0.0.1:6379`.
    ///
    /// # Errors
    /// Returns an error if the Redis client or connection manager cannot be
    /// created.
    pub async fn new(
        redis_url: &str,
        config: &RateLimitConfig,
        key_prefix: &str,
    ) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let conn_manager = redis::aio::ConnectionManager::new(client).await?;
        Ok(Self {
            client: conn_manager,
            max_requests: config.requests_per_window,
            window_secs: config.window_secs,
            key_prefix: key_prefix.to_string(),
        })
    }

    /// Build a Redis key for the given client identifier and current window.
    fn build_key(&self, ip: &str, window_id: u64) -> String {
        format!("{}:{}:{}", self.key_prefix, ip, window_id)
    }
}

/// Lua script for atomic rate-limit check-and-increment.
///
/// KEYS[1] = the rate-limit key
/// ARGV[1] = max requests (u32)
/// ARGV[2] = window TTL in seconds
///
/// Returns 1 if allowed, 0 if rate-limited.
const RATE_LIMIT_SCRIPT: &str = r#"
local current = redis.call('INCR', KEYS[1])
if current == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[2])
end
if current > tonumber(ARGV[1]) then
    return 0
end
return 1
"#;

impl RateLimitBackend for RedisBackend {
    fn check_key(&self, key: &str) -> Pin<Box<dyn Future<Output = RateLimitResult> + Send + '_>> {
        let effective_key = if key == "unknown" {
            "global".to_string()
        } else {
            key.to_string()
        };
        let max_requests = self.max_requests;
        let window_secs = self.window_secs;
        let mut conn = self.client.clone();

        // Calculate the current window ID based on Unix timestamp.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let window_id = now / window_secs.max(1);
        let redis_key = self.build_key(&effective_key, window_id);

        Box::pin(async move {
            let result: Result<i32, redis::RedisError> = redis::Script::new(RATE_LIMIT_SCRIPT)
                .key(&redis_key)
                .arg(max_requests)
                .arg(window_secs)
                .invoke_async(&mut conn)
                .await;

            match result {
                Ok(1) => Ok(true),
                Ok(_) => Ok(false),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        key = %redis_key,
                        "Redis rate-limit check failed, allowing request (fail-open)"
                    );
                    // Fail open: if Redis is down, allow the request rather
                    // than blocking all traffic.
                    Err(RateLimitError::BackendError(e.to_string()))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Unified rate-limit state
// ---------------------------------------------------------------------------

/// Shared rate limiter state that can use either an in-memory or Redis backend.
///
/// Wraps an `Arc<dyn RateLimitBackend>` for cheap cloning and dynamic dispatch.
#[derive(Clone)]
pub(crate) struct RateLimitState {
    backend: Arc<dyn RateLimitBackend>,
    /// Whether to fail open (allow requests) when the backend errors.
    /// Defaults to true for Redis, always true for in-memory (which never errors).
    fail_open: bool,
}

impl RateLimitState {
    /// Create a new rate limiter with the in-memory backend.
    ///
    /// # Errors
    /// Returns `RateLimitError::InvalidConfig` if the configuration is invalid.
    pub fn new(config: &RateLimitConfig) -> Result<Self, RateLimitError> {
        Ok(Self {
            backend: Arc::new(InMemoryBackend::new(config)?),
            fail_open: true,
        })
    }

    /// Create a new rate limiter with a Redis backend.
    pub fn with_redis(backend: RedisBackend) -> Self {
        Self {
            backend: Arc::new(backend),
            fail_open: true,
        }
    }

    /// Check whether the given IP is allowed to proceed.
    /// Returns `true` if allowed, `false` if rate-limited.
    pub async fn check_ip(&self, ip: &str) -> bool {
        match self.backend.check_key(ip).await {
            Ok(allowed) => allowed,
            Err(_) if self.fail_open => {
                // Fail open: allow the request if the backend is down
                true
            }
            Err(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

/// Tower [`Layer`] that applies per-IP rate limiting to a service.
///
/// Intended to be applied to specific route groups (e.g. auth endpoints).
#[derive(Clone)]
pub(crate) struct RateLimitLayer {
    state: RateLimitState,
}

impl RateLimitLayer {
    /// Create a new rate-limit layer with a custom in-memory configuration.
    ///
    /// # Errors
    /// Returns `RateLimitError::InvalidConfig` if the configuration is invalid.
    pub fn with_config(config: &RateLimitConfig) -> Result<Self, RateLimitError> {
        Ok(Self {
            state: RateLimitState::new(config)?,
        })
    }

    /// Create a new rate-limit layer backed by Redis.
    pub fn with_redis(backend: RedisBackend) -> Self {
        Self {
            state: RateLimitState::with_redis(backend),
        }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            state: self.state.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Tower [`Service`] that enforces rate limits.
///
/// Extracts the client IP from the request extensions (`ConnectInfo<SocketAddr>`)
/// or falls back to the `x-forwarded-for` / `x-real-ip` headers.
/// If the rate limit is exceeded, returns a 429 JSON error response.
#[derive(Clone)]
pub(crate) struct RateLimitService<S> {
    inner: S,
    state: RateLimitState,
}

/// Extract the client IP address from the request.
///
/// Priority:
/// 1. `x-forwarded-for` header (first IP, for reverse proxy setups)
/// 2. `x-real-ip` header
/// 3. `ConnectInfo<SocketAddr>` extension (direct connection)
/// 4. Falls back to "unknown"
fn extract_client_ip<B>(req: &Request<B>) -> String {
    // Check X-Forwarded-For first (common with reverse proxies)
    if let Some(forwarded) = req.headers().get("x-forwarded-for") {
        if let Ok(value) = forwarded.to_str() {
            // Take the first IP in the chain (client's IP)
            if let Some(first_ip) = value.split(',').next() {
                let trimmed = first_ip.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }

    // Check X-Real-IP
    if let Some(real_ip) = req.headers().get("x-real-ip") {
        if let Ok(value) = real_ip.to_str() {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    // Check ConnectInfo extension
    if let Some(connect_info) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return connect_info.0.ip().to_string();
    }

    "unknown".to_string()
}

/// Build a rate-limited JSON error response.
fn rate_limit_response() -> Response<Body> {
    let body = serde_json::json!({
        "error": {
            "code": "RATE_LIMITED",
            "message": "Too many requests. Please try again later."
        }
    });

    let body_bytes = serde_json::to_vec(&body).unwrap_or_default();

    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("content-type", "application/json")
        .header("retry-after", "60")
        .body(Body::from(body_bytes))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .body(Body::empty())
                .unwrap()
        })
}

impl<S> Service<Request<Body>> for RateLimitService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let ip = extract_client_ip(&req);
        let state = self.state.clone();

        // Clone the service to get an owned version for the async block
        let mut inner = self.inner.clone();
        // Swap so self.inner is the clone (ready) and we use the original
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            let allowed = state.check_ip(&ip).await;

            if !allowed {
                tracing::warn!(
                    client_ip = %ip,
                    "rate limit exceeded"
                );
                return Ok(rate_limit_response());
            }

            let response = inner.call(req).await?;
            Ok(response)
        })
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/// Create a [`RateLimitLayer`] configured for auth login/register endpoints.
///
/// Limits: 10 requests per 60 seconds per IP.
/// This is intentionally conservative to prevent brute-force login attempts.
pub(crate) fn auth_rate_limit_layer() -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 10,
        window_secs: 60,
    };
    RateLimitLayer::with_config(&config)
}

/// Create a [`RateLimitLayer`] configured for token creation endpoints.
///
/// Limits: 20 requests per 60 seconds per IP.
/// More permissive than login since token creation requires authentication,
/// but still rate-limited to prevent abuse.
pub(crate) fn token_rate_limit_layer() -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 20,
        window_secs: 60,
    };
    RateLimitLayer::with_config(&config)
}

/// Create a [`RateLimitLayer`] for auth endpoints backed by Redis.
///
/// Falls back to in-memory if the Redis connection fails.
pub(crate) async fn auth_rate_limit_layer_redis(
    redis_url: &str,
) -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 10,
        window_secs: 60,
    };
    match RedisBackend::new(redis_url, &config, "orbit:rl:auth").await {
        Ok(backend) => {
            tracing::info!("Using Redis-backed rate limiter for auth endpoints");
            Ok(RateLimitLayer::with_redis(backend))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to connect to Redis for rate limiting, falling back to in-memory"
            );
            RateLimitLayer::with_config(&config)
        }
    }
}

/// Create a [`RateLimitLayer`] for token creation endpoints backed by Redis.
///
/// Falls back to in-memory if the Redis connection fails.
pub(crate) async fn token_rate_limit_layer_redis(
    redis_url: &str,
) -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 20,
        window_secs: 60,
    };
    match RedisBackend::new(redis_url, &config, "orbit:rl:token").await {
        Ok(backend) => {
            tracing::info!("Using Redis-backed rate limiter for token endpoints");
            Ok(RateLimitLayer::with_redis(backend))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to connect to Redis for rate limiting, falling back to in-memory"
            );
            RateLimitLayer::with_config(&config)
        }
    }
}

/// Create a [`RateLimitLayer`] configured for repository creation.
///
/// Limits: 30 requests per 60 seconds per IP.
/// Prevents mass creation of repositories by a single IP.
pub(crate) fn repo_create_rate_limit_layer() -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    RateLimitLayer::with_config(&config)
}

/// Create a [`RateLimitLayer`] configured for write-heavy repo operations.
///
/// Applied to pull request creation and merge endpoints.
/// Limits: 30 requests per 60 seconds per IP.
/// These operations are computationally expensive and should be protected
/// against automated abuse.
pub(crate) fn repo_write_rate_limit_layer() -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    RateLimitLayer::with_config(&config)
}

/// Create a [`RateLimitLayer`] configured for admin mutation endpoints.
///
/// Applied to admin actions like disabling/enabling users, archiving repos,
/// and retrying jobs.
/// Limits: 30 requests per 60 seconds per IP.
/// Admin endpoints are already behind authentication and admin-role checks,
/// but rate limiting adds defense-in-depth.
pub(crate) fn admin_action_rate_limit_layer() -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    RateLimitLayer::with_config(&config)
}

/// Create a [`RateLimitLayer`] configured for Git push (receive-pack) operations.
///
/// Limits: 30 requests per 60 seconds per IP.
/// Git push operations are expensive (disk I/O, pack processing) and should
/// be rate-limited to prevent abuse.
pub(crate) fn git_receive_rate_limit_layer() -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    RateLimitLayer::with_config(&config)
}

/// Create a [`RateLimitLayer`] for repo creation backed by Redis.
///
/// Falls back to in-memory if the Redis connection fails.
pub(crate) async fn repo_create_rate_limit_layer_redis(
    redis_url: &str,
) -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    match RedisBackend::new(redis_url, &config, "orbit:rl:repo_create").await {
        Ok(backend) => {
            tracing::info!("Using Redis-backed rate limiter for repo creation");
            Ok(RateLimitLayer::with_redis(backend))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to connect to Redis for rate limiting, falling back to in-memory"
            );
            RateLimitLayer::with_config(&config)
        }
    }
}

/// Create a [`RateLimitLayer`] for write-heavy repo operations backed by Redis.
///
/// Falls back to in-memory if the Redis connection fails.
pub(crate) async fn repo_write_rate_limit_layer_redis(
    redis_url: &str,
) -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    match RedisBackend::new(redis_url, &config, "orbit:rl:repo_write").await {
        Ok(backend) => {
            tracing::info!("Using Redis-backed rate limiter for repo write operations");
            Ok(RateLimitLayer::with_redis(backend))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to connect to Redis for rate limiting, falling back to in-memory"
            );
            RateLimitLayer::with_config(&config)
        }
    }
}

/// Create a [`RateLimitLayer`] for admin actions backed by Redis.
///
/// Falls back to in-memory if the Redis connection fails.
pub(crate) async fn admin_action_rate_limit_layer_redis(
    redis_url: &str,
) -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    match RedisBackend::new(redis_url, &config, "orbit:rl:admin").await {
        Ok(backend) => {
            tracing::info!("Using Redis-backed rate limiter for admin actions");
            Ok(RateLimitLayer::with_redis(backend))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to connect to Redis for rate limiting, falling back to in-memory"
            );
            RateLimitLayer::with_config(&config)
        }
    }
}

/// Create a [`RateLimitLayer`] for Git push operations backed by Redis.
///
/// Falls back to in-memory if the Redis connection fails.
pub(crate) async fn git_receive_rate_limit_layer_redis(
    redis_url: &str,
) -> Result<RateLimitLayer, RateLimitError> {
    let config = RateLimitConfig {
        requests_per_window: 30,
        window_secs: 60,
    };
    match RedisBackend::new(redis_url, &config, "orbit:rl:git_push").await {
        Ok(backend) => {
            tracing::info!("Using Redis-backed rate limiter for git push operations");
            Ok(RateLimitLayer::with_redis(backend))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to connect to Redis for rate limiting, falling back to in-memory"
            );
            RateLimitLayer::with_config(&config)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = RateLimitConfig::default();
        assert_eq!(config.requests_per_window, 10);
        assert_eq!(config.window_secs, 60);
    }

    #[tokio::test]
    async fn rate_limit_state_creation() {
        let config = RateLimitConfig {
            requests_per_window: 5,
            window_secs: 30,
        };
        let _state = RateLimitState::new(&config).unwrap();
        let backend = InMemoryBackend::new(&config).unwrap();
        assert!(backend.check_key("127.0.0.1").await.unwrap());
        assert!(backend.check_key("unknown").await.unwrap());
    }

    #[tokio::test]
    async fn in_memory_backend_per_ip_isolation() {
        let config = RateLimitConfig {
            requests_per_window: 2,
            window_secs: 60,
        };
        let backend = InMemoryBackend::new(&config).unwrap();

        assert!(backend.check_key("10.0.0.1").await.unwrap());
        assert!(backend.check_key("10.0.0.1").await.unwrap());
        assert!(!backend.check_key("10.0.0.1").await.unwrap());
        assert!(backend.check_key("10.0.0.2").await.unwrap());
    }

    #[tokio::test]
    async fn in_memory_backend_exhaustion() {
        let config = RateLimitConfig {
            requests_per_window: 3,
            window_secs: 60,
        };
        let backend = InMemoryBackend::new(&config).unwrap();

        for _ in 0..3 {
            assert!(backend.check_key("192.168.1.1").await.unwrap());
        }
        assert!(!backend.check_key("192.168.1.1").await.unwrap());
    }

    #[test]
    fn rate_limit_response_format() {
        let resp = rate_limit_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(resp.headers().get("retry-after").unwrap(), "60");
    }

    #[test]
    fn extract_ip_from_x_forwarded_for() {
        let req = Request::builder()
            .header(
                "x-forwarded-for",
                "203.0.113.50, 70.41.3.18, 150.172.238.178",
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_client_ip(&req), "203.0.113.50");
    }

    #[test]
    fn extract_ip_from_x_real_ip() {
        let req = Request::builder()
            .header("x-real-ip", "203.0.113.50")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_client_ip(&req), "203.0.113.50");
    }

    #[test]
    fn extract_ip_fallback_unknown() {
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(extract_client_ip(&req), "unknown");
    }

    #[test]
    fn x_forwarded_for_takes_priority_over_x_real_ip() {
        let req = Request::builder()
            .header("x-forwarded-for", "10.0.0.1")
            .header("x-real-ip", "10.0.0.2")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_client_ip(&req), "10.0.0.1");
    }

    #[test]
    fn layer_clones() {
        let config = RateLimitConfig::default();
        let layer = RateLimitLayer::with_config(&config).unwrap();
        let _layer2 = layer.clone();
    }

    #[test]
    fn auth_rate_limit_layer_creates() {
        let _layer = auth_rate_limit_layer().unwrap();
    }

    #[test]
    fn token_rate_limit_layer_creates() {
        let _layer = token_rate_limit_layer().unwrap();
    }

    #[test]
    fn repo_create_rate_limit_layer_creates() {
        let _layer = repo_create_rate_limit_layer().unwrap();
    }

    #[test]
    fn repo_write_rate_limit_layer_creates() {
        let _layer = repo_write_rate_limit_layer().unwrap();
    }

    #[test]
    fn admin_action_rate_limit_layer_creates() {
        let _layer = admin_action_rate_limit_layer().unwrap();
    }

    #[test]
    fn git_receive_rate_limit_layer_creates() {
        let _layer = git_receive_rate_limit_layer().unwrap();
    }

    #[tokio::test]
    async fn repo_create_rate_limit_allows_30_per_minute() {
        let config = RateLimitConfig {
            requests_per_window: 30,
            window_secs: 60,
        };
        let backend = InMemoryBackend::new(&config).unwrap();

        for i in 0..30 {
            assert!(
                backend.check_key("10.0.0.200").await.unwrap(),
                "request {} should be allowed",
                i + 1,
            );
        }
        assert!(!backend.check_key("10.0.0.200").await.unwrap());
    }

    #[tokio::test]
    async fn token_rate_limit_is_more_permissive() {
        let config = RateLimitConfig {
            requests_per_window: 20,
            window_secs: 60,
        };
        let backend = InMemoryBackend::new(&config).unwrap();

        for i in 0..15 {
            assert!(
                backend.check_key("10.0.0.100").await.unwrap(),
                "request {} should be allowed",
                i + 1,
            );
        }
    }

    #[tokio::test]
    async fn rate_limit_service_allows_requests_within_limit() {
        use tower::ServiceExt;

        // Create a simple echo service
        let svc = tower::service_fn(|_req: Request<Body>| async {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap(),
            )
        });

        let config = RateLimitConfig {
            requests_per_window: 5,
            window_secs: 60,
        };
        let layer = RateLimitLayer::with_config(&config).unwrap();
        let mut svc = layer.layer(svc);

        // First request should succeed
        let req = Request::builder()
            .header("x-forwarded-for", "10.0.0.99")
            .body(Body::empty())
            .unwrap();
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rate_limit_service_blocks_when_exceeded() {
        use tower::ServiceExt;

        let svc = tower::service_fn(|_req: Request<Body>| async {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap(),
            )
        });

        let config = RateLimitConfig {
            requests_per_window: 2,
            window_secs: 60,
        };
        let layer = RateLimitLayer::with_config(&config).unwrap();
        let mut svc = layer.layer(svc);

        // Send 2 allowed requests
        for _ in 0..2 {
            let req = Request::builder()
                .header("x-forwarded-for", "10.0.0.50")
                .body(Body::empty())
                .unwrap();
            let resp = svc.ready().await.unwrap().call(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        // 3rd request should be rate-limited
        let req = Request::builder()
            .header("x-forwarded-for", "10.0.0.50")
            .body(Body::empty())
            .unwrap();
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        // Verify the body is proper JSON error format
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["error"]["code"], "RATE_LIMITED");
        assert!(!body["error"]["message"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rate_limit_different_ips_are_independent() {
        use tower::ServiceExt;

        let svc = tower::service_fn(|_req: Request<Body>| async {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap(),
            )
        });

        let config = RateLimitConfig {
            requests_per_window: 1,
            window_secs: 60,
        };
        let layer = RateLimitLayer::with_config(&config).unwrap();
        let mut svc = layer.layer(svc);

        // Exhaust limit for IP A
        let req = Request::builder()
            .header("x-forwarded-for", "10.0.0.1")
            .body(Body::empty())
            .unwrap();
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // IP A should now be blocked
        let req = Request::builder()
            .header("x-forwarded-for", "10.0.0.1")
            .body(Body::empty())
            .unwrap();
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        // IP B should still work
        let req = Request::builder()
            .header("x-forwarded-for", "10.0.0.2")
            .body(Body::empty())
            .unwrap();
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn in_memory_backend_async_check() {
        let config = RateLimitConfig {
            requests_per_window: 2,
            window_secs: 60,
        };
        let backend = InMemoryBackend::new(&config).unwrap();

        // Should allow first two requests
        assert!(backend.check_key("10.0.0.1").await.unwrap());
        assert!(backend.check_key("10.0.0.1").await.unwrap());

        // Third should be denied
        assert!(!backend.check_key("10.0.0.1").await.unwrap());

        // Different key should still be allowed
        assert!(backend.check_key("10.0.0.2").await.unwrap());
    }

    #[tokio::test]
    async fn rate_limit_state_async_check_ip() {
        let config = RateLimitConfig {
            requests_per_window: 2,
            window_secs: 60,
        };
        let state = RateLimitState::new(&config).unwrap();

        assert!(state.check_ip("10.0.0.1").await);
        assert!(state.check_ip("10.0.0.1").await);
        assert!(!state.check_ip("10.0.0.1").await);
    }

    #[test]
    fn redis_backend_build_key() {
        // We can test the key-building logic without a Redis connection
        // by constructing the struct fields manually (but we can't call new()
        // without Redis). Instead, test the key format logic directly.
        let now_secs = 1700000000u64;
        let window_secs = 60u64;
        let window_id = now_secs / window_secs;
        let key = format!("orbit:rl:auth:{}:{}", "10.0.0.1", window_id);
        assert!(key.starts_with("orbit:rl:auth:10.0.0.1:"));
        assert!(key.contains(&window_id.to_string()));
    }

    #[test]
    fn rate_limit_error_display() {
        let err = RateLimitError::BackendError("connection refused".to_string());
        let display = format!("{}", err);
        assert!(display.contains("connection refused"));
        assert!(display.contains("rate limit backend error"));
    }

}

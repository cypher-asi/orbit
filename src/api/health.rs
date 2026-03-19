use axum::{extract::State, Json};
use serde::Serialize;
use serde_json::{json, Value};

use crate::app_state::AppState;

/// Status of an individual health check component.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComponentStatus {
    /// The component is healthy and responding.
    Up,
    /// The component is not responding or returned an error.
    Down,
}

/// Health status of a single component with optional detail message.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComponentHealth {
    pub status: ComponentStatus,
    /// Optional human-readable detail (e.g. error message on failure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// How long to wait for Redis to respond before declaring it down.
const REDIS_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Perform a Redis PING to verify connectivity.
///
/// Returns `Ok(())` if Redis responds to PING within the timeout, or
/// `Err(message)` describing the failure.
async fn check_redis(redis_url: &str) -> Result<(), String> {
    // Wrap the entire check in a timeout so a hung Redis connection does
    // not block the health endpoint indefinitely.
    match tokio::time::timeout(REDIS_HEALTH_TIMEOUT, check_redis_inner(redis_url)).await {
        Ok(result) => result,
        Err(_) => Err("Redis health check timed out".to_string()),
    }
}

/// Inner implementation of the Redis health check (without timeout wrapper).
async fn check_redis_inner(redis_url: &str) -> Result<(), String> {
    let client = redis::Client::open(redis_url)
        .map_err(|e| format!("Failed to create Redis client: {}", e))?;

    let mut conn = redis::aio::ConnectionManager::new(client)
        .await
        .map_err(|e| format!("Failed to connect to Redis: {}", e))?;

    let pong: Result<String, redis::RedisError> = redis::cmd("PING").query_async(&mut conn).await;

    match pong {
        Ok(ref response) if response == "PONG" => Ok(()),
        Ok(other) => Err(format!("Unexpected PING response: {}", other)),
        Err(e) => Err(format!("Redis PING failed: {}", e)),
    }
}

/// GET /health - Returns 200 with health status.
///
/// When Redis is configured (via `REDIS_URL`), the response includes a
/// `components.redis` field reporting connectivity status. The overall
/// `status` is `"ok"` when all components are healthy, or `"degraded"`
/// when a non-critical component (like Redis) is unreachable.
///
/// ## Response format
///
/// ```json
/// {
///     "status": "ok",
///     "components": {
///         "redis": {
///             "status": "up"
///         }
///     }
/// }
/// ```
///
/// When Redis is not configured, the `components` object omits the `redis`
/// field entirely:
///
/// ```json
/// {
///     "status": "ok"
/// }
/// ```
pub(crate) async fn health_check(State(state): State<AppState>) -> Json<Value> {
    let mut components = serde_json::Map::new();
    let mut all_healthy = true;

    // Check Redis when configured for rate limiting.
    if let Some(ref redis_url) = state.config.redis_url {
        let redis_health = match check_redis(redis_url).await {
            Ok(()) => ComponentHealth {
                status: ComponentStatus::Up,
                message: None,
            },
            Err(msg) => {
                tracing::warn!(error = %msg, "Redis health check failed");
                all_healthy = false;
                ComponentHealth {
                    status: ComponentStatus::Down,
                    message: Some(msg),
                }
            }
        };
        components.insert(
            "redis".to_string(),
            serde_json::to_value(&redis_health).unwrap_or(json!({"status": "down"})),
        );
    }

    let status = if all_healthy { "ok" } else { "degraded" };

    if components.is_empty() {
        Json(json!({ "status": status }))
    } else {
        Json(json!({
            "status": status,
            "components": components
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::{
        body::to_bytes,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    /// Helper to build a minimal AppState for testing (no real DB).
    fn test_config_no_redis() -> Config {
        Config {
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
            aura_network_url: None,
        }
    }

    fn test_config_with_redis(url: &str) -> Config {
        Config {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 3000,
            git_storage_root: String::new(),
            log_level: String::new(),
            cors_allowed_origins: vec![],
            redis_url: Some(url.to_string()),
            public_base_url: None,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth_cookie_secret: String::new(),
            internal_service_token: String::new(),
            aura_network_url: None,
        }
    }

    /// Build a test AppState with a lazy DB pool (will not actually connect
    /// until a query is executed, but health check does not query the DB).
    fn test_app_state(config: Config) -> AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/orbit_health_test")
            .expect("failed to create lazy pool");
        AppState::new(pool, config)
    }

    #[tokio::test]
    async fn health_returns_ok_without_redis() {
        let state = test_app_state(test_config_no_redis());
        let app = Router::new()
            .route("/health", get(health_check))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("failed to read body");
        let value: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is not valid JSON");

        assert_eq!(value["status"], "ok");
        // No components field when Redis is not configured
        assert!(value.get("components").is_none());
    }

    #[tokio::test]
    async fn health_returns_degraded_when_redis_unreachable() {
        // Use a bogus Redis URL that will fail to connect
        let state = test_app_state(test_config_with_redis("redis://127.0.0.1:1"));
        let app = Router::new()
            .route("/health", get(health_check))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Still returns 200 -- the server itself is healthy, Redis is degraded
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("failed to read body");
        let value: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is not valid JSON");

        assert_eq!(value["status"], "degraded");
        assert_eq!(value["components"]["redis"]["status"], "down");
        // Should include an error message
        assert!(!value["components"]["redis"]["message"]
            .as_str()
            .unwrap_or("")
            .is_empty());
    }

    #[tokio::test]
    async fn health_includes_redis_component_when_configured() {
        // Even when Redis is down, the component should be present
        let state = test_app_state(test_config_with_redis("redis://127.0.0.1:1"));
        let app = Router::new()
            .route("/health", get(health_check))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("failed to read body");
        let value: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is not valid JSON");

        // The components.redis key should exist
        assert!(value["components"]["redis"].is_object());
        assert!(
            value["components"]["redis"]["status"] == "up"
                || value["components"]["redis"]["status"] == "down"
        );
    }

    #[test]
    fn component_health_serializes_up_without_message() {
        let health = ComponentHealth {
            status: ComponentStatus::Up,
            message: None,
        };
        let json = serde_json::to_value(&health).unwrap();
        assert_eq!(json, json!({"status": "up"}));
        // message field should be omitted (skip_serializing_if)
        assert!(json.get("message").is_none());
    }

    #[test]
    fn component_health_serializes_down_with_message() {
        let health = ComponentHealth {
            status: ComponentStatus::Down,
            message: Some("Connection refused".to_string()),
        };
        let json = serde_json::to_value(&health).unwrap();
        assert_eq!(json["status"], "down");
        assert_eq!(json["message"], "Connection refused");
    }
}

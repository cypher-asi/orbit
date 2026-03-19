//! Pagination support for list endpoints.
//!
//! Provides [`PaginationParams`] for extracting `limit`/`offset` query parameters,
//! [`PaginationMeta`] for response metadata, and [`PaginatedResponse`] for wrapping
//! list results in a consistent envelope.
//!
//! ## Defaults
//! - `limit`: 30 (max 100)
//! - `offset`: 0
//!
//! ## Response Format
//! ```json
//! {
//!     "data": [...],
//!     "pagination": {
//!         "total": 142,
//!         "limit": 30,
//!         "offset": 0
//!     }
//! }
//! ```

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default number of items per page.
const DEFAULT_LIMIT: u32 = 30;

/// Maximum allowed limit value.
const MAX_LIMIT: u32 = 100;

/// Default offset (start from the beginning).
const DEFAULT_OFFSET: u32 = 0;

// ---------------------------------------------------------------------------
// PaginationParams (query extractor)
// ---------------------------------------------------------------------------

/// Query parameters for paginated list endpoints.
///
/// Used with axum's `Query` extractor:
/// ```ignore
/// async fn list_items(
///     Query(params): Query<PaginationParams>,
/// ) -> impl IntoResponse { ... }
/// ```
///
/// Missing or out-of-range values are clamped to safe defaults.
#[derive(Debug, Clone, Deserialize)]
pub struct PaginationParams {
    /// Number of items to return. Defaults to 30, max 100.
    pub limit: Option<u32>,
    /// Number of items to skip. Defaults to 0.
    pub offset: Option<u32>,
}

impl PaginationParams {
    /// Resolve the effective limit, clamped to [1, MAX_LIMIT].
    pub fn limit(&self) -> u32 {
        self.limit.unwrap_or(DEFAULT_LIMIT).max(1).min(MAX_LIMIT)
    }

    /// Resolve the effective offset.
    pub fn offset(&self) -> u32 {
        self.offset.unwrap_or(DEFAULT_OFFSET)
    }

    /// Convert to a resolved [`Pagination`] with clamped values.
    pub fn into_pagination(self) -> Pagination {
        Pagination {
            limit: self.limit(),
            offset: self.offset(),
        }
    }
}

impl Default for PaginationParams {
    fn default() -> Self {
        Self {
            limit: None,
            offset: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Pagination (resolved values)
// ---------------------------------------------------------------------------

/// Resolved pagination values with defaults applied and limits clamped.
#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    /// Number of items to return (1..=100).
    pub limit: u32,
    /// Number of items to skip.
    pub offset: u32,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            limit: DEFAULT_LIMIT,
            offset: DEFAULT_OFFSET,
        }
    }
}

impl Pagination {
    /// Create pagination with specific values.
    pub fn new(limit: u32, offset: u32) -> Self {
        Self {
            limit: limit.max(1).min(MAX_LIMIT),
            offset,
        }
    }

    /// Get limit as i64 (convenient for SQL queries).
    pub fn limit_i64(&self) -> i64 {
        i64::from(self.limit)
    }

    /// Get offset as i64 (convenient for SQL queries).
    pub fn offset_i64(&self) -> i64 {
        i64::from(self.offset)
    }
}

// ---------------------------------------------------------------------------
// PaginationMeta (response metadata)
// ---------------------------------------------------------------------------

/// Pagination metadata included in paginated responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaginationMeta {
    /// Total number of items matching the query (across all pages).
    pub total: i64,
    /// Number of items returned in this page.
    pub limit: u32,
    /// Number of items skipped.
    pub offset: u32,
}

// ---------------------------------------------------------------------------
// PaginatedResponse<T>
// ---------------------------------------------------------------------------

/// A paginated response envelope wrapping a list of items with metadata.
///
/// Serializes to:
/// ```json
/// {
///     "data": [...],
///     "pagination": { "total": 142, "limit": 30, "offset": 0 }
/// }
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    /// The page of items.
    pub data: Vec<T>,
    /// Pagination metadata.
    pub pagination: PaginationMeta,
}

impl<T: Serialize> PaginatedResponse<T> {
    /// Create a new paginated response.
    pub fn new(data: Vec<T>, total: i64, limit: u32, offset: u32) -> Self {
        Self {
            data,
            pagination: PaginationMeta {
                total,
                limit,
                offset,
            },
        }
    }

    /// Create a paginated response from a [`Pagination`] and total count.
    pub fn from_pagination(data: Vec<T>, total: i64, pagination: &Pagination) -> Self {
        Self::new(data, total, pagination.limit, pagination.offset)
    }
}

impl<T: Serialize> IntoResponse for PaginatedResponse<T> {
    fn into_response(self) -> Response {
        let json_body = match serde_json::to_vec(&self) {
            Ok(bytes) => bytes,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({
                        "error": {
                            "code": "INTERNAL_ERROR",
                            "message": "Failed to serialize response",
                            "details": null
                        }
                    })),
                )
                    .into_response();
            }
        };

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json_body))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn pagination_params_defaults() {
        let params = PaginationParams::default();
        assert_eq!(params.limit(), DEFAULT_LIMIT);
        assert_eq!(params.offset(), DEFAULT_OFFSET);
    }

    #[test]
    fn pagination_params_custom_values() {
        let params = PaginationParams {
            limit: Some(50),
            offset: Some(10),
        };
        assert_eq!(params.limit(), 50);
        assert_eq!(params.offset(), 10);
    }

    #[test]
    fn pagination_params_clamps_limit_to_max() {
        let params = PaginationParams {
            limit: Some(200),
            offset: None,
        };
        assert_eq!(params.limit(), MAX_LIMIT);
    }

    #[test]
    fn pagination_params_clamps_limit_to_min() {
        let params = PaginationParams {
            limit: Some(0),
            offset: None,
        };
        assert_eq!(params.limit(), 1);
    }

    #[test]
    fn pagination_params_into_pagination() {
        let params = PaginationParams {
            limit: Some(50),
            offset: Some(20),
        };
        let p = params.into_pagination();
        assert_eq!(p.limit, 50);
        assert_eq!(p.offset, 20);
    }

    #[test]
    fn pagination_default() {
        let p = Pagination::default();
        assert_eq!(p.limit, DEFAULT_LIMIT);
        assert_eq!(p.offset, DEFAULT_OFFSET);
    }

    #[test]
    fn pagination_new_clamps() {
        let p = Pagination::new(500, 10);
        assert_eq!(p.limit, MAX_LIMIT);
        assert_eq!(p.offset, 10);

        let p = Pagination::new(0, 0);
        assert_eq!(p.limit, 1);
    }

    #[test]
    fn pagination_i64_conversions() {
        let p = Pagination::new(30, 60);
        assert_eq!(p.limit_i64(), 30);
        assert_eq!(p.offset_i64(), 60);
    }

    #[test]
    fn pagination_meta_serializes() {
        let meta = PaginationMeta {
            total: 142,
            limit: 30,
            offset: 0,
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["total"], 142);
        assert_eq!(json["limit"], 30);
        assert_eq!(json["offset"], 0);
    }

    #[test]
    fn pagination_meta_deserializes() {
        let json = serde_json::json!({"total": 50, "limit": 10, "offset": 5});
        let meta: PaginationMeta = serde_json::from_value(json).unwrap();
        assert_eq!(meta.total, 50);
        assert_eq!(meta.limit, 10);
        assert_eq!(meta.offset, 5);
    }

    #[test]
    fn paginated_response_serializes() {
        let resp = PaginatedResponse::new(vec!["item1", "item2"], 42, 30, 0);
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["data"], serde_json::json!(["item1", "item2"]));
        assert_eq!(json["pagination"]["total"], 42);
        assert_eq!(json["pagination"]["limit"], 30);
        assert_eq!(json["pagination"]["offset"], 0);
    }

    #[test]
    fn paginated_response_from_pagination() {
        let pagination = Pagination::new(10, 20);
        let resp = PaginatedResponse::from_pagination(vec![1, 2, 3], 100, &pagination);
        assert_eq!(resp.data, vec![1, 2, 3]);
        assert_eq!(resp.pagination.total, 100);
        assert_eq!(resp.pagination.limit, 10);
        assert_eq!(resp.pagination.offset, 20);
    }

    #[test]
    fn paginated_response_empty_data() {
        let resp: PaginatedResponse<String> = PaginatedResponse::new(vec![], 0, 30, 0);
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["data"], serde_json::json!([]));
        assert_eq!(json["pagination"]["total"], 0);
    }

    #[tokio::test]
    async fn paginated_response_into_response() {
        let resp = PaginatedResponse::new(vec!["a", "b", "c"], 10, 30, 0);
        let response = resp.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );

        let body_bytes = to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("failed to read body");
        let body: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is not valid JSON");
        assert_eq!(body["data"], serde_json::json!(["a", "b", "c"]));
        assert_eq!(body["pagination"]["total"], 10);
        assert_eq!(body["pagination"]["limit"], 30);
        assert_eq!(body["pagination"]["offset"], 0);
    }

    #[test]
    fn pagination_params_deserializes_from_query() {
        // Simulating query string deserialization
        let json = serde_json::json!({"limit": 50, "offset": 10});
        let params: PaginationParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.limit, Some(50));
        assert_eq!(params.offset, Some(10));
    }

    #[test]
    fn pagination_params_deserializes_empty() {
        let json = serde_json::json!({});
        let params: PaginationParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.limit, None);
        assert_eq!(params.offset, None);
    }
}

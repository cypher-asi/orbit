//! Standardized response helpers for the API.
//!
//! Provides consistent response formatting including:
//! - [`SuccessResponse`] for wrapping single-item success responses
//! - [`created`] helper for 201 Created responses
//! - [`no_content`] helper for 204 No Content responses
//! - [`ok`] helper for 200 OK responses

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

// ---------------------------------------------------------------------------
// SuccessResponse<T>
// ---------------------------------------------------------------------------

/// A standardized success response wrapper.
///
/// Wraps data in a `{"data": ...}` envelope for consistency with the
/// paginated response format.
#[derive(Debug, Clone, Serialize)]
pub struct SuccessResponse<T: Serialize> {
    /// The response payload.
    pub data: T,
}

impl<T: Serialize> SuccessResponse<T> {
    /// Create a new success response wrapping the given data.
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

impl<T: Serialize> IntoResponse for SuccessResponse<T> {
    fn into_response(self) -> Response {
        (StatusCode::OK, axum::Json(self)).into_response()
    }
}

// ---------------------------------------------------------------------------
// CreatedResponse<T>
// ---------------------------------------------------------------------------

/// A 201 Created response wrapper.
#[derive(Debug, Clone, Serialize)]
pub struct CreatedResponse<T: Serialize> {
    /// The newly created resource.
    pub data: T,
}

impl<T: Serialize> CreatedResponse<T> {
    /// Create a new 201 Created response wrapping the given data.
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

impl<T: Serialize> IntoResponse for CreatedResponse<T> {
    fn into_response(self) -> Response {
        (StatusCode::CREATED, axum::Json(self)).into_response()
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Return a 200 OK response wrapping the data in `{"data": ...}`.
pub fn ok<T: Serialize>(data: T) -> SuccessResponse<T> {
    SuccessResponse::new(data)
}

/// Return a 201 Created response wrapping the data in `{"data": ...}`.
pub fn created<T: Serialize>(data: T) -> CreatedResponse<T> {
    CreatedResponse::new(data)
}

/// Return a 204 No Content response with an empty body.
pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn response_to_parts(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let body_bytes = to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("failed to read body");
        if body_bytes.is_empty() {
            return (status, serde_json::Value::Null);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is not valid JSON");
        (status, value)
    }

    #[tokio::test]
    async fn success_response_wraps_data() {
        let resp = SuccessResponse::new("hello");
        let (status, body) = response_to_parts(resp.into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"data": "hello"}));
    }

    #[tokio::test]
    async fn success_response_with_struct() {
        #[derive(Serialize)]
        struct Item {
            id: u32,
            name: String,
        }
        let item = Item {
            id: 1,
            name: "test".to_string(),
        };
        let resp = ok(item);
        let (status, body) = response_to_parts(resp.into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["id"], 1);
        assert_eq!(body["data"]["name"], "test");
    }

    #[tokio::test]
    async fn created_response_returns_201() {
        let resp = created("new-item");
        let (status, body) = response_to_parts(resp.into_response()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body, serde_json::json!({"data": "new-item"}));
    }

    #[tokio::test]
    async fn no_content_returns_204() {
        let resp = no_content();
        let (status, body) = response_to_parts(resp).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(body, serde_json::Value::Null);
    }

    #[test]
    fn success_response_serializes() {
        let resp = SuccessResponse::new(vec![1, 2, 3]);
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json, serde_json::json!({"data": [1, 2, 3]}));
    }

    #[test]
    fn created_response_serializes() {
        let resp = CreatedResponse::new("item");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json, serde_json::json!({"data": "item"}));
    }
}

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Unified API error type that maps to appropriate HTTP responses.
///
/// Every variant produces a JSON body of the form:
/// ```json
/// {"error": {"code": "...", "message": "...", "details": ...}}
/// ```
#[derive(Debug)]
#[allow(dead_code)]
pub enum ApiError {
    /// 400 Bad Request
    BadRequest(String),
    /// 401 Unauthorized
    Unauthorized(String),
    /// 403 Forbidden
    Forbidden(String),
    /// 404 Not Found
    NotFound(String),
    /// 409 Conflict
    Conflict(String),
    /// 422 Unprocessable Entity
    Unprocessable(String),
    /// 429 Too Many Requests
    RateLimited(String),
    /// 500 Internal Server Error
    Internal(String),
}

impl ApiError {
    /// Return the HTTP status code for this error variant.
    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Return a short, machine-readable error code string.
    ///
    /// These follow the uppercase convention from the API spec:
    /// `VALIDATION_ERROR`, `UNAUTHORIZED`, `FORBIDDEN`, `NOT_FOUND`,
    /// `CONFLICT`, `UNPROCESSABLE`, `RATE_LIMITED`, `INTERNAL_ERROR`.
    fn code(&self) -> &'static str {
        match self {
            ApiError::BadRequest(_) => "VALIDATION_ERROR",
            ApiError::Unauthorized(_) => "UNAUTHORIZED",
            ApiError::Forbidden(_) => "FORBIDDEN",
            ApiError::NotFound(_) => "NOT_FOUND",
            ApiError::Conflict(_) => "CONFLICT",
            ApiError::Unprocessable(_) => "UNPROCESSABLE",
            ApiError::RateLimited(_) => "RATE_LIMITED",
            ApiError::Internal(_) => "INTERNAL_ERROR",
        }
    }

    /// Return the human-readable error message.
    fn message(&self) -> &str {
        match self {
            ApiError::BadRequest(msg)
            | ApiError::Unauthorized(msg)
            | ApiError::Forbidden(msg)
            | ApiError::NotFound(msg)
            | ApiError::Conflict(msg)
            | ApiError::Unprocessable(msg)
            | ApiError::RateLimited(msg)
            | ApiError::Internal(msg) => msg.as_str(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = json!({
            "error": {
                "code": self.code(),
                "message": self.message(),
                "details": null
            }
        });

        (status, axum::Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        // Log the underlying database error at error level for diagnostics.
        tracing::error!(error = %err, "database error");

        match err {
            sqlx::Error::RowNotFound => ApiError::NotFound("resource not found".to_string()),
            sqlx::Error::Database(ref db_err) => {
                // PostgreSQL unique-violation SQLSTATE: 23505
                if db_err.code().as_deref() == Some("23505") {
                    ApiError::Conflict("resource already exists".to_string())
                } else {
                    ApiError::Internal("internal server error".to_string())
                }
            }
            _ => ApiError::Internal("internal server error".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::StatusCode;

    /// Helper: convert an `ApiError` to `(StatusCode, serde_json::Value)`.
    async fn error_to_parts(err: ApiError) -> (StatusCode, serde_json::Value) {
        let response = err.into_response();
        let status = response.status();
        let body_bytes = to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("failed to read response body");
        let value: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body is not valid JSON");
        (status, value)
    }

    #[tokio::test]
    async fn bad_request_response() {
        let (status, body) = error_to_parts(ApiError::BadRequest("invalid input".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({"error": {"code": "VALIDATION_ERROR", "message": "invalid input", "details": null}})
        );
    }

    #[tokio::test]
    async fn unauthorized_response() {
        let (status, body) = error_to_parts(ApiError::Unauthorized("missing token".into())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body,
            json!({"error": {"code": "UNAUTHORIZED", "message": "missing token", "details": null}})
        );
    }

    #[tokio::test]
    async fn forbidden_response() {
        let (status, body) = error_to_parts(ApiError::Forbidden("access denied".into())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body,
            json!({"error": {"code": "FORBIDDEN", "message": "access denied", "details": null}})
        );
    }

    #[tokio::test]
    async fn not_found_response() {
        let (status, body) = error_to_parts(ApiError::NotFound("no such thing".into())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            json!({"error": {"code": "NOT_FOUND", "message": "no such thing", "details": null}})
        );
    }

    #[tokio::test]
    async fn conflict_response() {
        let (status, body) = error_to_parts(ApiError::Conflict("already exists".into())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body,
            json!({"error": {"code": "CONFLICT", "message": "already exists", "details": null}})
        );
    }

    #[tokio::test]
    async fn unprocessable_response() {
        let (status, body) = error_to_parts(ApiError::Unprocessable("bad entity".into())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body,
            json!({"error": {"code": "UNPROCESSABLE", "message": "bad entity", "details": null}})
        );
    }

    #[tokio::test]
    async fn rate_limited_response() {
        let (status, body) = error_to_parts(ApiError::RateLimited("slow down".into())).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            body,
            json!({"error": {"code": "RATE_LIMITED", "message": "slow down", "details": null}})
        );
    }

    #[tokio::test]
    async fn internal_response() {
        let (status, body) = error_to_parts(ApiError::Internal("something broke".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"error": {"code": "INTERNAL_ERROR", "message": "something broke", "details": null}})
        );
    }

    #[tokio::test]
    async fn error_response_has_details_field() {
        let (_, body) = error_to_parts(ApiError::BadRequest("test".into())).await;
        // Verify the details field is present (even if null)
        assert!(body["error"].get("details").is_some());
    }

    #[tokio::test]
    async fn from_sqlx_row_not_found() {
        let err: ApiError = sqlx::Error::RowNotFound.into();
        let (status, body) = error_to_parts(err).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "NOT_FOUND");
    }

    #[test]
    fn display_impl() {
        let err = ApiError::BadRequest("oops".into());
        assert_eq!(format!("{}", err), "VALIDATION_ERROR: oops");
    }
}

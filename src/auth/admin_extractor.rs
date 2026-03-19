use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::app_state::AppState;
use crate::errors::ApiError;
use crate::users::models::User;

use super::AuthUser;

/// Extractor that resolves an authenticated admin user from a Bearer token.
///
/// Wraps `AuthUser` and additionally checks that `is_admin` is `true`.
/// Returns `ApiError::Forbidden` if the authenticated user is not an admin.
pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let AuthUser(user) = AuthUser::from_request_parts(parts, state).await?;

        if !user.is_admin {
            return Err(ApiError::Forbidden(
                "admin access required".to_string(),
            ));
        }

        Ok(AdminUser(user))
    }
}

use std::path::PathBuf;

use sqlx::postgres::PgPool;

use crate::auth::jwt::TokenValidator;
use crate::config::Config;

/// Shared application state passed to request handlers via axum's `State` extractor.
#[derive(Clone)]
pub struct AppState {
    /// PostgreSQL connection pool.
    pub db: PgPool,
    /// Application configuration.
    pub config: Config,
    /// Resolved path to the Git bare-repository storage root.
    pub git_storage_root: PathBuf,
    /// JWT token validator (Auth0 JWKS RS256 + HS256).
    pub token_validator: TokenValidator,
}

impl AppState {
    /// Build a new `AppState` from the provided components.
    pub fn new(db: PgPool, config: Config) -> Self {
        let git_storage_root = PathBuf::from(&config.git_storage_root);
        let token_validator = TokenValidator::new(
            config.auth0_domain.clone(),
            config.auth0_audience.clone(),
            config.auth_cookie_secret.clone(),
        );
        Self {
            db,
            config,
            git_storage_root,
            token_validator,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `AppState` derives `Clone` (compile-time check).
    #[test]
    fn app_state_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<AppState>();
    }
}

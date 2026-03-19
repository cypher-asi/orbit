use std::path::PathBuf;

use sqlx::postgres::PgPool;

use crate::config::Config;

/// Shared application state passed to request handlers via axum's `State` extractor.
#[derive(Clone)]
#[allow(dead_code)]
pub struct AppState {
    /// PostgreSQL connection pool.
    pub db: PgPool,
    /// Application configuration.
    pub config: Config,
    /// Resolved path to the Git bare-repository storage root.
    pub git_storage_root: PathBuf,
}

impl AppState {
    /// Build a new `AppState` from the provided components.
    pub fn new(db: PgPool, config: Config) -> Self {
        let git_storage_root = PathBuf::from(&config.git_storage_root);
        Self {
            db,
            config,
            git_storage_root,
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

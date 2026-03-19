use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

/// Create a PostgreSQL connection pool with sensible defaults.
///
/// Uses `connect_lazy` so the pool is created immediately without
/// waiting for an active connection. Connections are established
/// on demand when the first query is executed.
///
/// - Maximum 10 connections
/// - Acquire timeout of 30 seconds
pub fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(30))
        .connect_lazy(database_url)
}

/// Run all pending SQL migrations from the `migrations/` directory.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

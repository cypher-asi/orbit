mod admin;
mod api;
mod app_state;
mod auth;
mod branches;
mod commits;
mod config;
mod db;
mod errors;
mod events;
mod git_http;
mod jobs;
mod merge_engine;
mod permissions;
mod pull_requests;
mod repos;
mod storage;
mod users;

use std::fs;
use std::path::PathBuf;

use tokio::sync::watch;

#[tokio::main]
async fn main() {
    // Load configuration from env / .env file
    let config = config::Config::load();

    // Initialize tracing/logging (JSON in production, pretty otherwise)
    events::logging::init_logging(&config.log_level);

    tracing::info!("Creating database connection pool");
    let pool = db::create_pool(&config.database_url)
        .expect("Failed to create database pool");

    // Attempt to run database migrations; log a warning if the database
    // is not yet reachable so the server can still start for health checks.
    tracing::info!("Running database migrations");
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        db::run_migrations(&pool),
    )
    .await
    {
        Ok(Ok(_)) => tracing::info!("Database migrations completed successfully"),
        Ok(Err(e)) => tracing::warn!(error = %e, "Failed to run database migrations -- the server will start but database features may be unavailable"),
        Err(_) => tracing::warn!("Database migration timed out -- the server will start but database features may be unavailable"),
    }

    // Ensure git storage root directory exists
    let git_storage_path = std::path::Path::new(&config.git_storage_root);
    if !git_storage_path.exists() {
        fs::create_dir_all(git_storage_path)
            .expect("Failed to create git storage root directory");
        tracing::info!(path = %config.git_storage_root, "Created git storage root directory");
    }

    // Build shared application state
    let state = app_state::AppState::new(pool, config.clone());

    // Clone the pool for the background worker before state is moved into the router.
    let worker_pool = state.db.clone();

    // Build router via the central router composition
    let app = api::router::build_router(state).await;

    let bind_addr = format!("{}:{}", config.server_host, config.server_port);
    tracing::info!(address = %bind_addr, "Orbit server starting");

    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, address = %bind_addr, "Failed to bind to configured address, trying port 0");
            let fallback_addr = format!("{}:0", config.server_host);
            tokio::net::TcpListener::bind(&fallback_addr)
                .await
                .expect("Failed to bind TCP listener on fallback port")
        }
    };

    let actual_addr = listener.local_addr().expect("Failed to get local address");
    tracing::info!(address = %actual_addr, "Orbit server listening");

    // Create a shutdown signal channel for graceful shutdown.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Spawn the background job worker.
    let worker_storage = storage::service::StorageConfig::new(
        PathBuf::from(&config.git_storage_root),
    );
    let worker_handle = tokio::spawn(jobs::worker::run_worker(
        worker_pool,
        worker_storage,
        shutdown_rx,
    ));

    // Run the HTTP server with graceful shutdown on Ctrl+C.
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl+C");
            tracing::info!("received Ctrl+C, initiating graceful shutdown");
            // Signal the worker to stop.
            let _ = shutdown_tx.send(true);
        });

    server.await.expect("Server error");

    // Wait for the worker to finish its current job and exit.
    tracing::info!("waiting for background job worker to shut down");
    let _ = worker_handle.await;
    tracing::info!("Orbit server shut down");
}

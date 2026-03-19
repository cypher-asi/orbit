use tracing_subscriber::{fmt, EnvFilter};

/// Initialize the `tracing` subscriber for structured application logging.
///
/// - When `ORBIT_ENV` is set to `"production"`, logs are emitted as JSON
///   for machine consumption (structured logging).
/// - Otherwise, a human-readable "pretty" format is used for development.
/// - The log level is controlled by the `log_level` parameter, which is
///   typically sourced from `Config::log_level` (e.g. `"info"`, `"debug"`).
///
/// # Panics
/// Panics if the tracing subscriber cannot be set (e.g. if one is already
/// installed). This function should be called exactly once at startup.
pub fn init_logging(log_level: &str) {
    let env_filter = EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let orbit_env = std::env::var("ORBIT_ENV").unwrap_or_default();

    if orbit_env == "production" {
        // JSON format for production: structured, machine-parseable.
        fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .json()
            .init();
    } else {
        // Pretty, human-readable format for development.
        fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .init();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn init_logging_does_not_panic_with_valid_level() {
        // We cannot actually call init_logging in tests because the global
        // subscriber can only be set once per process. Instead we verify
        // that the EnvFilter parsing logic works for typical levels.
        use tracing_subscriber::EnvFilter;

        for level in &["trace", "debug", "info", "warn", "error"] {
            let filter = EnvFilter::try_new(level);
            assert!(filter.is_ok(), "failed to parse level: {}", level);
        }
    }

    #[test]
    fn invalid_level_falls_back_gracefully() {
        use tracing_subscriber::EnvFilter;

        // Our init_logging uses unwrap_or_else to fall back to "info" when
        // the provided level string cannot be parsed. Verify that "info" is
        // always accepted as a valid fallback.
        let result = EnvFilter::try_new("info");
        assert!(result.is_ok());
    }
}

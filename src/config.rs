use std::env;

use anyhow::Context;

/// Application configuration loaded from environment variables.
///
/// Supports `.env` file fallback via `dotenvy`.
#[derive(Debug, Clone)]
pub struct Config {
    /// PostgreSQL connection URL.
    pub database_url: String,
    /// Host address to bind the HTTP server to.
    pub server_host: String,
    /// Port to bind the HTTP server to.
    pub server_port: u16,
    /// Root directory for bare Git repository storage.
    pub git_storage_root: String,
    /// Logging level filter (e.g. "info", "debug", "warn").
    pub log_level: String,
    /// Comma-separated list of allowed CORS origins.
    ///
    /// When empty (the default), any origin is allowed (permissive /
    /// development mode). In production, set `CORS_ORIGINS` to a
    /// comma-separated list of origins, e.g.
    /// `https://example.com,https://app.example.com`.
    pub cors_allowed_origins: Vec<String>,
    /// Optional Redis URL for distributed rate limiting.
    ///
    /// When set, the rate limiter uses Redis as a shared backend so that
    /// rate-limit state is consistent across multiple server instances and
    /// survives restarts. When unset (the default), the in-memory governor
    /// rate limiter is used instead.
    ///
    /// Example: `redis://127.0.0.1:6379`
    pub redis_url: Option<String>,
    /// Optional public base URL for discovery and links (e.g. `https://orbit.example.com`).
    ///
    /// When set, the discovery endpoint uses this for `base_url` and `git_url_prefix`.
    /// When unset, falls back to `http://{server_host}:{server_port}` for local development.
    pub public_base_url: Option<String>,
    /// Auth0 domain for JWKS key fetching (e.g. `auth.zero.tech`).
    pub auth0_domain: String,
    /// Auth0 audience for RS256 token validation.
    pub auth0_audience: String,
    /// Shared secret for HS256 token validation (same as aura-network/storage).
    pub auth_cookie_secret: String,
    /// Token for service-to-service auth (X-Internal-Token header).
    pub internal_service_token: String,
    /// aura-network base URL for integration lookups (e.g. `https://aura-network.onrender.com`).
    /// Required for GitHub mirror — orbit queries aura-network for org integration config.
    pub aura_network_url: Option<String>,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Attempts to read a `.env` file first (silently ignored if absent),
    /// then reads each variable from the environment. Variables with
    /// defaults are optional; the rest are required.
    ///
    /// # Errors
    /// Returns an error if required variables are missing or invalid.
    pub fn load() -> anyhow::Result<Self> {
        // Load .env file if present; ignore errors (file may not exist).
        let _ = dotenvy::dotenv();

        let database_url = env::var("DATABASE_URL").context("DATABASE_URL must be set")?;

        let server_host = env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

        let server_port = env::var("SERVER_PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse::<u16>()
            .context("SERVER_PORT must be a valid u16")?;

        let git_storage_root =
            env::var("GIT_STORAGE_ROOT").unwrap_or_else(|_| "./data/repos".to_string());

        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());

        let cors_allowed_origins: Vec<String> = env::var("CORS_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let redis_url = env::var("REDIS_URL").ok().filter(|s| !s.is_empty());

        let public_base_url = env::var("PUBLIC_BASE_URL").ok().filter(|s| !s.is_empty());

        let auth0_domain = env::var("AUTH0_DOMAIN").context("AUTH0_DOMAIN must be set")?;
        let auth0_audience = env::var("AUTH0_AUDIENCE").context("AUTH0_AUDIENCE must be set")?;
        let auth_cookie_secret =
            env::var("AUTH_COOKIE_SECRET").context("AUTH_COOKIE_SECRET must be set")?;
        let internal_service_token =
            env::var("INTERNAL_SERVICE_TOKEN").context("INTERNAL_SERVICE_TOKEN must be set")?;

        let aura_network_url = env::var("AURA_NETWORK_URL").ok().filter(|s| !s.is_empty());

        Ok(Config {
            database_url,
            server_host,
            server_port,
            git_storage_root,
            log_level,
            cors_allowed_origins,
            redis_url,
            public_base_url,
            auth0_domain,
            auth0_audience,
            auth_cookie_secret,
            internal_service_token,
            aura_network_url,
        })
    }

    /// Base URL for the REST API and Git clone (with trailing slash removed).
    /// Used by the discovery endpoint.
    pub fn base_url(&self) -> String {
        self.public_base_url
            .clone()
            .unwrap_or_else(|| format!("http://{}:{}", self.server_host, self.server_port))
            .trim_end_matches('/')
            .to_string()
    }

    /// Prefix for Git clone URLs: `{git_url_prefix}{org_id}/{repo}.git`.
    pub fn git_url_prefix(&self) -> String {
        format!("{}/", self.base_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    #[serial_test::serial]
    fn load_with_defaults() {
        // SAFETY: Tests are run with --test-threads=1 or accept the
        // inherent race when mutating the environment in tests.
        unsafe {
            // Set required variables
            env::set_var("DATABASE_URL", "postgres://localhost/orbit_test");
            env::set_var("GIT_STORAGE_ROOT", "/tmp/orbit_repos");
            env::set_var("AUTH0_DOMAIN", "test.auth0.com");
            env::set_var("AUTH0_AUDIENCE", "orbit-test");
            env::set_var("AUTH_COOKIE_SECRET", "test-secret");
            env::set_var("INTERNAL_SERVICE_TOKEN", "test-token");

            // Remove optional variables so defaults kick in
            env::remove_var("SERVER_HOST");
            env::remove_var("SERVER_PORT");
            env::remove_var("LOG_LEVEL");
            env::remove_var("CORS_ORIGINS");
            env::remove_var("REDIS_URL");
            env::remove_var("PUBLIC_BASE_URL");
        }

        let config = Config::load().expect("load with defaults");

        assert_eq!(config.database_url, "postgres://localhost/orbit_test");
        assert_eq!(config.server_host, "0.0.0.0");
        assert_eq!(config.server_port, 3000);
        assert_eq!(config.git_storage_root, "/tmp/orbit_repos");
        assert_eq!(config.log_level, "info");
        assert!(config.cors_allowed_origins.is_empty());
        assert!(config.redis_url.is_none());
        assert!(config.public_base_url.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn load_with_custom_values() {
        // SAFETY: See note in load_with_defaults. Set all vars in one block
        // immediately before load() to reduce the window for parallel tests.
        unsafe {
            env::set_var("DATABASE_URL", "postgres://db:5432/orbit");
            env::set_var("SERVER_HOST", "127.0.0.1");
            env::set_var("AUTH0_DOMAIN", "test.auth0.com");
            env::set_var("AUTH0_AUDIENCE", "orbit-test");
            env::set_var("AUTH_COOKIE_SECRET", "test-secret");
            env::set_var("INTERNAL_SERVICE_TOKEN", "test-token");
            env::set_var("SERVER_PORT", "8080");
            env::set_var("GIT_STORAGE_ROOT", "/data/repos");
            env::set_var("LOG_LEVEL", "debug");
            env::set_var(
                "CORS_ORIGINS",
                "https://example.com, https://app.example.com",
            );
            env::remove_var("PUBLIC_BASE_URL");
        }
        let config = Config::load().expect("load with custom values");

        assert_eq!(config.database_url, "postgres://db:5432/orbit");
        assert_eq!(config.server_host, "127.0.0.1");
        assert_eq!(config.server_port, 8080);
        assert_eq!(config.git_storage_root, "/data/repos");
        assert_eq!(config.log_level, "debug");
        assert_eq!(
            config.cors_allowed_origins,
            vec!["https://example.com", "https://app.example.com"],
        );
    }
}

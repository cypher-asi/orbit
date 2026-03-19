use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

const SELF_SIGNED_KID: &str = "jFNXMnFjGrSoDafnLQBohoCNalWcFcTjnKEbkRzWFBHyYJFikdLMHP";
const JWKS_CACHE_TTL: Duration = Duration::from_secs(300); // 5 minutes

#[derive(Debug, Clone, Deserialize)]
pub struct TokenClaims {
    pub id: Option<String>,
    pub sub: Option<String>,
}

impl TokenClaims {
    pub fn user_id(&self) -> Option<&str> {
        self.id.as_deref().or(self.sub.as_deref())
    }
}

#[derive(Clone)]
pub struct TokenValidator {
    jwks: JwksClient,
    cookie_secret: String,
    auth0_domain: String,
    auth0_audience: String,
}

impl TokenValidator {
    pub fn new(auth0_domain: String, auth0_audience: String, cookie_secret: String) -> Self {
        Self {
            jwks: JwksClient::new(&auth0_domain),
            cookie_secret,
            auth0_domain,
            auth0_audience,
        }
    }

    pub async fn validate(&self, token: &str) -> Result<TokenClaims, String> {
        let header =
            jsonwebtoken::decode_header(token).map_err(|e| format!("Invalid token header: {e}"))?;

        let kid = header.kid.as_deref().unwrap_or("");

        if kid == SELF_SIGNED_KID {
            self.validate_hs256(token)
        } else {
            self.validate_rs256(token, kid).await
        }
    }

    fn validate_hs256(&self, token: &str) -> Result<TokenClaims, String> {
        let key = DecodingKey::from_secret(self.cookie_secret.as_bytes());
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();

        decode::<TokenClaims>(token, &key, &validation)
            .map(|data| data.claims)
            .map_err(|e| format!("HS256 validation failed: {e}"))
    }

    async fn validate_rs256(&self, token: &str, kid: &str) -> Result<TokenClaims, String> {
        let key = self.jwks.get_key(kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.auth0_audience]);
        validation.set_issuer(&[format!("https://{}/", self.auth0_domain)]);

        decode::<TokenClaims>(token, &key, &validation)
            .map(|data| data.claims)
            .map_err(|e| format!("RS256 validation failed: {e}"))
    }
}

// ---------------------------------------------------------------------------
// JWKS Client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkKey>,
}

#[derive(Debug, Deserialize)]
struct JwkKey {
    kid: String,
    n: String,
    e: String,
}

struct CacheState {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Instant,
}

#[derive(Clone)]
struct JwksClient {
    jwks_url: String,
    http: reqwest::Client,
    cache: Arc<RwLock<Option<CacheState>>>,
}

impl JwksClient {
    fn new(auth0_domain: &str) -> Self {
        Self {
            jwks_url: format!("https://{auth0_domain}/.well-known/jwks.json"),
            http: reqwest::Client::new(),
            cache: Arc::new(RwLock::new(None)),
        }
    }

    async fn get_key(&self, kid: &str) -> Result<DecodingKey, String> {
        {
            let cache = self.cache.read().await;
            if let Some(ref state) = *cache {
                if state.fetched_at.elapsed() < JWKS_CACHE_TTL {
                    if let Some(key) = state.keys.get(kid) {
                        return Ok(key.clone());
                    }
                }
            }
        }
        self.refresh_and_get(kid).await
    }

    async fn refresh_and_get(&self, kid: &str) -> Result<DecodingKey, String> {
        let resp = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|e| format!("JWKS fetch failed: {e}"))?;

        let jwks: JwksResponse = resp
            .json()
            .await
            .map_err(|e| format!("JWKS parse failed: {e}"))?;

        let mut keys = HashMap::new();
        for key in &jwks.keys {
            if let Ok(decoding_key) = DecodingKey::from_rsa_components(&key.n, &key.e) {
                keys.insert(key.kid.clone(), decoding_key);
            }
        }

        let result = keys
            .get(kid)
            .cloned()
            .ok_or_else(|| format!("Key ID '{kid}' not found in JWKS"));

        let mut cache = self.cache.write().await;
        *cache = Some(CacheState {
            keys,
            fetched_at: Instant::now(),
        });

        result
    }
}

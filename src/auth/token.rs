use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Generate a cryptographically random token.
///
/// Generates 32 random bytes using `OsRng`, then returns a tuple of:
/// - `raw_base64url`: the raw token as a base64url-encoded string (no padding)
/// - `sha256_hex_hash`: the SHA-256 hex digest of the raw base64url string
///
/// The raw token is returned to the user exactly once. Only the hash is stored
/// in the database for later verification.
pub fn generate_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let raw_base64url = URL_SAFE_NO_PAD.encode(bytes);
    let sha256_hex = hash_token(&raw_base64url);
    (raw_base64url, sha256_hex)
}

/// Compute the SHA-256 hex digest of a raw token string.
///
/// This is used both during token creation (to derive the stored hash) and
/// during token verification (to look up the hash in the database).
pub fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_returns_valid_base64url_and_hash() {
        let (raw, hash) = generate_token();

        // raw should be base64url-encoded 32 bytes => 43 chars (no padding)
        assert_eq!(raw.len(), 43);

        // hash should be a 64-char lowercase hex string (SHA-256)
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // hash should match hash_token(raw)
        assert_eq!(hash, hash_token(&raw));
    }

    #[test]
    fn generate_token_is_unique() {
        let (raw1, hash1) = generate_token();
        let (raw2, hash2) = generate_token();
        assert_ne!(raw1, raw2);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn hash_token_is_deterministic() {
        let input = "test-token-value";
        let h1 = hash_token(input);
        let h2 = hash_token(input);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn hash_token_produces_correct_sha256() {
        // Known SHA-256 of "hello"
        let hash = hash_token("hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn raw_token_decodes_to_32_bytes() {
        let (raw, _) = generate_token();
        let decoded = URL_SAFE_NO_PAD.decode(&raw).expect("should decode");
        assert_eq!(decoded.len(), 32);
    }
}

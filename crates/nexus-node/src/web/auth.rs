//! Authentication for the embedded web panel.
//!
//! The panel exposes destructive, root-equivalent operations (container
//! lifecycle, arbitrary file read/write, backups, in-container command
//! execution). This module gates every `/api/v1/*` route behind a login:
//!
//! - `POST /api/v1/auth/login` exchanges an admin password (or API key) for a
//!   short-lived opaque session token.
//! - An Axum middleware validates the `Authorization: Bearer <token>` header
//!   (or `nexus_session` cookie) on every protected request.
//!
//! When authentication is enabled but no credential is configured the
//! middleware **fails closed** — it rejects every protected request rather than
//! silently serving them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::RwLock;

/// Configuration for panel authentication, sourced from environment variables.
#[derive(Debug, Clone)]
pub struct WebAuthConfig {
    /// Master switch (shared with the gRPC auth via `AUTH_ENABLED`).
    pub enabled: bool,
    /// Admin password (compared in constant time). `None` if unset/empty.
    pub password: Option<String>,
    /// SHA-256 hashes of accepted API keys (`AUTH_API_KEYS`, comma-separated).
    pub api_key_hashes: Vec<String>,
    /// How long an issued session token remains valid.
    pub session_ttl: Duration,
}

impl Default for WebAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            password: None,
            api_key_hashes: Vec::new(),
            session_ttl: Duration::from_secs(86_400),
        }
    }
}

impl WebAuthConfig {
    /// Build the configuration from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("AUTH_ENABLED") {
            let e = enabled.to_lowercase();
            config.enabled = e == "true" || e == "1";
        }

        config.password = std::env::var("AUTH_PASSWORD").ok().filter(|p| !p.is_empty());

        if let Ok(keys) = std::env::var("AUTH_API_KEYS") {
            config.api_key_hashes = keys
                .split(',')
                .map(|k| k.trim())
                .filter(|k| !k.is_empty())
                .map(sha256_hex)
                .collect();
        }

        if let Ok(ttl) = std::env::var("WEB_SESSION_TTL_SECS") {
            if let Ok(secs) = ttl.parse::<u64>() {
                if secs > 0 {
                    config.session_ttl = Duration::from_secs(secs);
                }
            }
        }

        config
    }

    /// Whether at least one usable credential is configured.
    pub fn has_credentials(&self) -> bool {
        self.password.is_some() || !self.api_key_hashes.is_empty()
    }

    /// Verify a plaintext password against the configured one (constant time).
    pub fn verify_password(&self, candidate: &str) -> bool {
        match &self.password {
            Some(expected) => ct_eq(expected.as_bytes(), candidate.as_bytes()),
            None => false,
        }
    }

    /// Verify an API key against the configured hashes (constant time).
    pub fn verify_api_key(&self, candidate: &str) -> bool {
        let candidate_hash = sha256_hex(candidate);
        self.api_key_hashes
            .iter()
            .any(|h| ct_eq(h.as_bytes(), candidate_hash.as_bytes()))
    }
}

/// In-memory store of active session tokens with expiry.
pub struct SessionStore {
    sessions: RwLock<HashMap<String, SystemTime>>,
    ttl: Duration,
}

impl SessionStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// Create a new session and return its opaque token.
    pub async fn create(&self) -> String {
        let token = generate_token();
        let expiry = SystemTime::now() + self.ttl;
        let mut sessions = self.sessions.write().await;
        purge_expired(&mut sessions);
        sessions.insert(token.clone(), expiry);
        token
    }

    /// Return true if the token exists and has not expired.
    pub async fn validate(&self, token: &str) -> bool {
        let sessions = self.sessions.read().await;
        match sessions.get(token) {
            Some(expiry) => *expiry > SystemTime::now(),
            None => false,
        }
    }

    /// Remove a session token (logout).
    pub async fn revoke(&self, token: &str) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(token);
    }

    /// Seconds until an issued token expires.
    pub fn ttl_secs(&self) -> u64 {
        self.ttl.as_secs()
    }
}

fn purge_expired(sessions: &mut HashMap<String, SystemTime>) {
    let now = SystemTime::now();
    sessions.retain(|_, expiry| *expiry > now);
}

/// Generate a 256-bit random token, hex-encoded.
fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// SHA-256 of the input, hex-encoded.
fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(input.as_bytes());
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Constant-time byte comparison. The length comparison itself is not secret.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extract a bearer token from an `Authorization` header value.
pub fn bearer_from_header(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
        Some(token)
    } else {
        None
    }
}

/// Extract the `nexus_session` value from a `Cookie` header.
pub fn session_from_cookie(cookie_header: &str) -> Option<&str> {
    cookie_header.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        if name == "nexus_session" && !value.is_empty() {
            Some(value)
        } else {
            None
        }
    })
}

/// Shared handles the middleware and handlers need.
#[derive(Clone)]
pub struct AuthState {
    pub config: Arc<WebAuthConfig>,
    pub sessions: Arc<SessionStore>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_matches_std_eq() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreT"));
        assert!(!ct_eq(b"secret", b"secret-longer"));
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn password_verification() {
        let cfg = WebAuthConfig {
            enabled: true,
            password: Some("hunter2".to_string()),
            ..Default::default()
        };
        assert!(cfg.verify_password("hunter2"));
        assert!(!cfg.verify_password("hunter3"));
        assert!(cfg.has_credentials());
    }

    #[test]
    fn api_key_verification() {
        let cfg = WebAuthConfig {
            enabled: true,
            api_key_hashes: vec![sha256_hex("my-key")],
            ..Default::default()
        };
        assert!(cfg.verify_api_key("my-key"));
        assert!(!cfg.verify_api_key("wrong-key"));
        assert!(cfg.has_credentials());
    }

    #[test]
    fn no_credentials_detected() {
        let cfg = WebAuthConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(!cfg.has_credentials());
        assert!(!cfg.verify_password("anything"));
    }

    #[tokio::test]
    async fn session_lifecycle() {
        let store = SessionStore::new(Duration::from_secs(60));
        let token = store.create().await;
        assert!(store.validate(&token).await);
        store.revoke(&token).await;
        assert!(!store.validate(&token).await);
        assert!(!store.validate("nonexistent").await);
    }

    #[tokio::test]
    async fn expired_session_rejected() {
        let store = SessionStore::new(Duration::from_millis(1));
        let token = store.create().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!store.validate(&token).await);
    }

    #[test]
    fn header_and_cookie_parsing() {
        assert_eq!(bearer_from_header("Bearer abc123"), Some("abc123"));
        assert_eq!(bearer_from_header("bearer abc123"), Some("abc123"));
        assert_eq!(bearer_from_header("Basic abc123"), None);
        assert_eq!(bearer_from_header("Bearer "), None);

        assert_eq!(
            session_from_cookie("foo=bar; nexus_session=tok123; baz=qux"),
            Some("tok123")
        );
        assert_eq!(session_from_cookie("foo=bar"), None);
    }
}

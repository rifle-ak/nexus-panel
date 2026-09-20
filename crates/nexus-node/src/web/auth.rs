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
//!
//! Sessions carry a [`SessionScope`]. A password or API key login is an
//! `Admin` session; a session minted from a billing-system SSO token is
//! scoped to the one server the customer is paying for, and the middleware
//! refuses everything else on the node for it.

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

/// What a session is allowed to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionScope {
    /// Full control of the node: the operator's own login or an API key.
    Admin,
    /// A customer signed in through the billing system: these servers only,
    /// and nothing at node level.
    Servers(Vec<String>),
}

impl SessionScope {
    pub fn is_admin(&self) -> bool {
        matches!(self, SessionScope::Admin)
    }

    /// Whether this session may act on `server_id`.
    pub fn allows_server(&self, server_id: &str) -> bool {
        match self {
            SessionScope::Admin => true,
            SessionScope::Servers(ids) => ids.iter().any(|id| id == server_id),
        }
    }
}

#[derive(Debug, Clone)]
struct Session {
    expiry: SystemTime,
    scope: SessionScope,
}

/// In-memory store of active session tokens with expiry.
pub struct SessionStore {
    sessions: RwLock<HashMap<String, Session>>,
    ttl: Duration,
}

impl SessionStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// Create a new admin session and return its opaque token.
    pub async fn create(&self) -> String {
        self.create_scoped(SessionScope::Admin, None).await
    }

    /// Create a session with the given scope. `ttl` shorter than the store's
    /// default is honoured; anything longer is clamped to the default, so a
    /// caller cannot mint a session that outlives what the operator
    /// configured.
    pub async fn create_scoped(&self, scope: SessionScope, ttl: Option<Duration>) -> String {
        let token = generate_token();
        let ttl = ttl.map(|t| t.min(self.ttl)).unwrap_or(self.ttl);
        let expiry = SystemTime::now() + ttl;
        let mut sessions = self.sessions.write().await;
        purge_expired(&mut sessions);
        sessions.insert(token.clone(), Session { expiry, scope });
        token
    }

    /// Return true if the token exists and has not expired.
    pub async fn validate(&self, token: &str) -> bool {
        self.scope_of(token).await.is_some()
    }

    /// The scope of a live session, or `None` for an unknown/expired token.
    pub async fn scope_of(&self, token: &str) -> Option<SessionScope> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(token)?;
        if session.expiry > SystemTime::now() {
            Some(session.scope.clone())
        } else {
            None
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

/// Default lifetime of a single sign-on token: long enough for a browser
/// redirect, short enough that a leaked link is useless by the time anyone
/// reads it.
pub const SSO_TOKEN_DEFAULT_TTL: Duration = Duration::from_secs(60);
/// The longest an SSO token may be asked to live.
pub const SSO_TOKEN_MAX_TTL: Duration = Duration::from_secs(300);

/// What an SSO token grants once redeemed.
#[derive(Debug, Clone)]
pub struct SsoGrant {
    /// The server the customer is signing in to.
    pub server_id: String,
    /// Who the billing system says this is (opaque; for audit logs).
    pub subject: Option<String>,
    /// Requested session lifetime, if the caller wants it shorter than the
    /// store default.
    pub session_ttl: Option<Duration>,
    expiry: SystemTime,
}

/// One-time tokens the billing system mints so a customer can land in the
/// panel without a password.
///
/// The token travels in a URL, so only its hash is kept here; a copy of this
/// process's memory does not yield usable links. Redeeming a token consumes
/// it: a link works exactly once.
pub struct SsoTokenStore {
    tokens: RwLock<HashMap<String, SsoGrant>>,
}

impl Default for SsoTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SsoTokenStore {
    pub fn new() -> Self {
        Self {
            tokens: RwLock::new(HashMap::new()),
        }
    }

    /// Mint a token for `server_id` and return it (the only time the raw
    /// value exists outside the URL it is put in).
    pub async fn issue(
        &self,
        server_id: &str,
        subject: Option<String>,
        ttl: Duration,
        session_ttl: Option<Duration>,
    ) -> String {
        let token = generate_token();
        let ttl = ttl.clamp(Duration::from_secs(1), SSO_TOKEN_MAX_TTL);
        let grant = SsoGrant {
            server_id: server_id.to_string(),
            subject,
            session_ttl,
            expiry: SystemTime::now() + ttl,
        };
        let mut tokens = self.tokens.write().await;
        let now = SystemTime::now();
        tokens.retain(|_, g| g.expiry > now);
        tokens.insert(sha256_hex(&token), grant);
        token
    }

    /// Consume a token. Returns its grant if it existed and had not expired;
    /// either way the token is gone afterwards.
    pub async fn redeem(&self, token: &str) -> Option<SsoGrant> {
        let mut tokens = self.tokens.write().await;
        let grant = tokens.remove(&sha256_hex(token))?;
        if grant.expiry > SystemTime::now() {
            Some(grant)
        } else {
            None
        }
    }
}

fn purge_expired(sessions: &mut HashMap<String, Session>) {
    let now = SystemTime::now();
    sessions.retain(|_, s| s.expiry > now);
}

/// Generate a 256-bit random token, hex-encoded.
fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// SHA-256 of the input, hex-encoded.
pub(crate) fn sha256_hex(input: &str) -> String {
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

    #[tokio::test]
    async fn scoped_session_reports_its_scope() {
        let store = SessionStore::new(Duration::from_secs(60));
        let admin = store.create().await;
        assert_eq!(store.scope_of(&admin).await, Some(SessionScope::Admin));

        let scope = SessionScope::Servers(vec!["srv-1".into()]);
        let scoped = store.create_scoped(scope.clone(), None).await;
        assert_eq!(store.scope_of(&scoped).await, Some(scope.clone()));
        assert!(scope.allows_server("srv-1"));
        assert!(!scope.allows_server("srv-2"));
        assert!(!scope.is_admin());
        assert!(SessionScope::Admin.allows_server("anything"));
    }

    #[tokio::test]
    async fn scoped_session_ttl_cannot_exceed_store_ttl() {
        // A billing system asking for a week-long session gets the operator's
        // configured maximum instead.
        let store = SessionStore::new(Duration::from_millis(20));
        let token = store
            .create_scoped(
                SessionScope::Servers(vec!["s".into()]),
                Some(Duration::from_secs(3600)),
            )
            .await;
        assert!(store.validate(&token).await);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!store.validate(&token).await);
    }

    #[tokio::test]
    async fn sso_token_is_single_use() {
        let store = SsoTokenStore::new();
        let token = store
            .issue(
                "srv-1",
                Some("client-7".into()),
                SSO_TOKEN_DEFAULT_TTL,
                None,
            )
            .await;
        let grant = store.redeem(&token).await.expect("first redemption works");
        assert_eq!(grant.server_id, "srv-1");
        assert_eq!(grant.subject.as_deref(), Some("client-7"));
        assert!(store.redeem(&token).await.is_none(), "second use must fail");
        assert!(store.redeem("not-a-token").await.is_none());
    }

    #[tokio::test]
    async fn sso_token_expires() {
        let store = SsoTokenStore::new();
        // `issue` clamps to at least one second, so expire it by hand.
        let token = store.issue("srv-1", None, Duration::from_secs(1), None).await;
        {
            let mut tokens = store.tokens.write().await;
            for grant in tokens.values_mut() {
                grant.expiry = SystemTime::now() - Duration::from_secs(1);
            }
        }
        assert!(store.redeem(&token).await.is_none());
    }
}

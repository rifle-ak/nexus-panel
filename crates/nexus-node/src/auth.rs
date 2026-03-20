//! Authentication and Authorization middleware for enterprise gRPC services.
//!
//! Supports multiple authentication methods:
//! - API Key authentication (X-API-Key header)
//! - JWT Bearer token authentication
//! - mTLS client certificate authentication
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::auth::{AuthConfig, AuthInterceptor};
//!
//! let config = AuthConfig::from_env();
//! let interceptor = AuthInterceptor::new(config);
//! ```

use jsonwebtoken::{decode, DecodingKey, TokenData, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tonic::{metadata::MetadataMap, Request, Status};
use tracing::{debug, error, info, warn};

/// Authentication errors
#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Missing authentication credentials")]
    MissingCredentials,

    #[error("Invalid API key")]
    InvalidApiKey,

    #[error("Invalid JWT token: {0}")]
    InvalidToken(String),

    #[error("Token expired")]
    TokenExpired,

    #[error("Insufficient permissions: required {required}, found {found:?}")]
    InsufficientPermissions {
        required: String,
        found: Vec<String>,
    },

    #[error("Authentication method not allowed")]
    MethodNotAllowed,

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Internal authentication error: {0}")]
    Internal(String),
}

impl From<AuthError> for Status {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::MissingCredentials => {
                Status::unauthenticated("Missing authentication credentials")
            }
            AuthError::InvalidApiKey => Status::unauthenticated("Invalid API key"),
            AuthError::InvalidToken(msg) => {
                Status::unauthenticated(format!("Invalid token: {}", msg))
            }
            AuthError::TokenExpired => Status::unauthenticated("Token expired"),
            AuthError::InsufficientPermissions { required, found } => Status::permission_denied(
                format!("Required permission '{}', found {:?}", required, found),
            ),
            AuthError::MethodNotAllowed => {
                Status::unauthenticated("Authentication method not allowed")
            }
            AuthError::RateLimitExceeded => {
                Status::resource_exhausted("Rate limit exceeded, please retry later")
            }
            AuthError::Internal(msg) => Status::internal(format!("Auth error: {}", msg)),
        }
    }
}

/// JWT Claims structure
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    /// Subject (user ID or service ID)
    pub sub: String,
    /// Issuer
    pub iss: String,
    /// Audience
    pub aud: Option<String>,
    /// Expiration time (Unix timestamp)
    pub exp: u64,
    /// Issued at (Unix timestamp)
    pub iat: u64,
    /// Not before (Unix timestamp)
    pub nbf: Option<u64>,
    /// JWT ID (unique identifier)
    pub jti: Option<String>,
    /// Roles/permissions
    #[serde(default)]
    pub roles: Vec<String>,
    /// Scopes
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Node ID (for node-specific tokens)
    pub node_id: Option<String>,
    /// Tenant ID (for multi-tenant deployments)
    pub tenant_id: Option<String>,
}

impl Claims {
    /// Check if the claims have a specific role
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role || r == "admin")
    }

    /// Check if the claims have a specific scope
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == "*")
    }

    /// Check if the token is expired
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        now > self.exp
    }

    /// Check if the token is not yet valid
    pub fn is_not_yet_valid(&self) -> bool {
        if let Some(nbf) = self.nbf {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            now < nbf
        } else {
            false
        }
    }
}

/// Authentication methods supported
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMethod {
    /// API Key in X-API-Key header
    ApiKey,
    /// JWT Bearer token
    BearerToken,
    /// mTLS client certificate
    ClientCertificate,
    /// No authentication (for health checks, metrics)
    None,
}

/// Authentication configuration
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Enabled authentication methods
    pub enabled_methods: HashSet<AuthMethod>,
    /// API keys (hashed with SHA256)
    pub api_key_hashes: Vec<String>,
    /// JWT secret key for HMAC algorithms
    pub jwt_secret: Option<String>,
    /// JWT public key for RSA/EC algorithms
    pub jwt_public_key: Option<String>,
    /// Expected JWT issuer
    pub jwt_issuer: Option<String>,
    /// Expected JWT audience
    pub jwt_audience: Option<String>,
    /// Token validity leeway (seconds)
    pub token_leeway: u64,
    /// Methods that bypass authentication (e.g., health checks)
    pub bypass_methods: HashSet<String>,
    /// Required scopes per method
    pub method_scopes: std::collections::HashMap<String, Vec<String>>,
    /// Enable authentication (master switch)
    pub enabled: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        let mut enabled_methods = HashSet::new();
        enabled_methods.insert(AuthMethod::ApiKey);
        enabled_methods.insert(AuthMethod::BearerToken);

        let mut bypass_methods = HashSet::new();
        bypass_methods.insert("/nexus.node.v1.NodeService/HealthCheck".to_string());
        bypass_methods.insert("/grpc.health.v1.Health/Check".to_string());

        Self {
            enabled_methods,
            api_key_hashes: Vec::new(),
            jwt_secret: None,
            jwt_public_key: None,
            jwt_issuer: None,
            jwt_audience: None,
            token_leeway: 60,
            bypass_methods,
            method_scopes: std::collections::HashMap::new(),
            enabled: false, // Disabled by default for backward compatibility
        }
    }
}

impl AuthConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let mut config = Self::default();

        // Master enable switch
        if let Ok(enabled) = std::env::var("AUTH_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }

        // API Keys (comma-separated, will be hashed)
        if let Ok(keys) = std::env::var("AUTH_API_KEYS") {
            config.api_key_hashes = keys.split(',').map(|k| Self::hash_api_key(k.trim())).collect();
        }

        // JWT configuration
        config.jwt_secret = std::env::var("AUTH_JWT_SECRET").ok();
        config.jwt_public_key = std::env::var("AUTH_JWT_PUBLIC_KEY").ok();
        config.jwt_issuer = std::env::var("AUTH_JWT_ISSUER").ok();
        config.jwt_audience = std::env::var("AUTH_JWT_AUDIENCE").ok();

        if let Ok(leeway) = std::env::var("AUTH_TOKEN_LEEWAY") {
            config.token_leeway = leeway.parse().unwrap_or(60);
        }

        // Bypass methods
        if let Ok(methods) = std::env::var("AUTH_BYPASS_METHODS") {
            for method in methods.split(',') {
                config.bypass_methods.insert(method.trim().to_string());
            }
        }

        config
    }

    /// Hash an API key using SHA256
    pub fn hash_api_key(key: &str) -> String {
        use sha2::Digest;
        let hash = sha2::Sha256::digest(key.as_bytes());
        hex::encode(hash)
    }

    /// Add a pre-hashed API key
    pub fn add_api_key_hash(&mut self, hash: String) {
        self.api_key_hashes.push(hash);
    }

    /// Add an API key (will be hashed)
    pub fn add_api_key(&mut self, key: &str) {
        self.api_key_hashes.push(Self::hash_api_key(key));
    }

    /// Set required scopes for a gRPC method
    pub fn require_scope(&mut self, method: &str, scopes: Vec<String>) {
        self.method_scopes.insert(method.to_string(), scopes);
    }
}

/// Authenticated identity
#[derive(Debug, Clone)]
pub struct Identity {
    /// Subject identifier
    pub subject: String,
    /// Authentication method used
    pub method: AuthMethod,
    /// Roles
    pub roles: Vec<String>,
    /// Scopes
    pub scopes: Vec<String>,
    /// Tenant ID (for multi-tenant)
    pub tenant_id: Option<String>,
    /// Node ID (if specified in token)
    pub node_id: Option<String>,
    /// Raw claims (if JWT)
    pub claims: Option<Claims>,
}

impl Identity {
    /// Create an anonymous identity
    pub fn anonymous() -> Self {
        Self {
            subject: "anonymous".to_string(),
            method: AuthMethod::None,
            roles: Vec::new(),
            scopes: Vec::new(),
            tenant_id: None,
            node_id: None,
            claims: None,
        }
    }

    /// Create identity from API key
    pub fn from_api_key(key_identifier: &str) -> Self {
        Self {
            subject: format!("api-key:{}", key_identifier),
            method: AuthMethod::ApiKey,
            roles: vec!["api-user".to_string()],
            scopes: vec!["*".to_string()], // API keys have full access
            tenant_id: None,
            node_id: None,
            claims: None,
        }
    }

    /// Create identity from JWT claims
    pub fn from_claims(claims: Claims) -> Self {
        Self {
            subject: claims.sub.clone(),
            method: AuthMethod::BearerToken,
            roles: claims.roles.clone(),
            scopes: claims.scopes.clone(),
            tenant_id: claims.tenant_id.clone(),
            node_id: claims.node_id.clone(),
            claims: Some(claims),
        }
    }

    /// Check if identity has a role
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role || r == "admin")
    }

    /// Check if identity has a scope
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == "*")
    }
}

/// Authentication interceptor for gRPC
#[derive(Clone)]
pub struct AuthInterceptor {
    config: Arc<AuthConfig>,
}

impl AuthInterceptor {
    /// Create a new authentication interceptor
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Authenticate a request
    pub fn authenticate(
        &self,
        metadata: &MetadataMap,
        method: &str,
    ) -> Result<Identity, AuthError> {
        // Check if authentication is enabled
        if !self.config.enabled {
            debug!("Authentication disabled, allowing request");
            return Ok(Identity::anonymous());
        }

        // Check if method bypasses authentication
        if self.config.bypass_methods.contains(method) {
            debug!("Method {} bypasses authentication", method);
            return Ok(Identity::anonymous());
        }

        // Try API key authentication
        if self.config.enabled_methods.contains(&AuthMethod::ApiKey) {
            if let Some(api_key) = metadata.get("x-api-key") {
                return self.authenticate_api_key(
                    api_key.to_str().map_err(|_e| AuthError::InvalidApiKey)?,
                );
            }
        }

        // Try Bearer token authentication
        if self.config.enabled_methods.contains(&AuthMethod::BearerToken) {
            if let Some(auth_header) = metadata.get("authorization") {
                let auth_str = auth_header
                    .to_str()
                    .map_err(|_| AuthError::InvalidToken("Invalid header encoding".to_string()))?;

                if auth_str.to_lowercase().starts_with("bearer ") {
                    let token = &auth_str[7..];
                    return self.authenticate_jwt(token);
                }
            }
        }

        // No valid credentials found
        Err(AuthError::MissingCredentials)
    }

    /// Authenticate using API key
    fn authenticate_api_key(&self, api_key: &str) -> Result<Identity, AuthError> {
        let key_hash = AuthConfig::hash_api_key(api_key);

        if self.config.api_key_hashes.contains(&key_hash) {
            info!("API key authentication successful");
            // Use first 8 chars of hash as identifier
            Ok(Identity::from_api_key(&key_hash[..8]))
        } else {
            warn!("Invalid API key attempt");
            Err(AuthError::InvalidApiKey)
        }
    }

    /// Authenticate using JWT token
    fn authenticate_jwt(&self, token: &str) -> Result<Identity, AuthError> {
        let secret = self
            .config
            .jwt_secret
            .as_ref()
            .ok_or_else(|| AuthError::Internal("JWT secret not configured".to_string()))?;

        let mut validation = Validation::default();
        validation.leeway = self.config.token_leeway;

        if let Some(ref issuer) = self.config.jwt_issuer {
            validation.set_issuer(&[issuer]);
        }

        if let Some(ref audience) = self.config.jwt_audience {
            validation.set_audience(&[audience]);
        }

        let key = DecodingKey::from_secret(secret.as_bytes());
        let token_data: TokenData<Claims> =
            decode(token, &key, &validation).map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
                _ => AuthError::InvalidToken(e.to_string()),
            })?;

        let claims = token_data.claims;

        // Additional validation
        if claims.is_expired() {
            return Err(AuthError::TokenExpired);
        }

        if claims.is_not_yet_valid() {
            return Err(AuthError::InvalidToken("Token not yet valid".to_string()));
        }

        info!("JWT authentication successful for subject: {}", claims.sub);
        Ok(Identity::from_claims(claims))
    }

    /// Check if identity has required scopes for method
    pub fn check_authorization(&self, identity: &Identity, method: &str) -> Result<(), AuthError> {
        if let Some(required_scopes) = self.config.method_scopes.get(method) {
            for scope in required_scopes {
                if !identity.has_scope(scope) {
                    return Err(AuthError::InsufficientPermissions {
                        required: scope.clone(),
                        found: identity.scopes.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Tower service layer for authentication
pub mod layer {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::body::BoxBody;
    use tower::{Layer, Service};

    /// Authentication layer
    #[derive(Clone)]
    pub struct AuthLayer {
        interceptor: AuthInterceptor,
    }

    impl AuthLayer {
        pub fn new(config: AuthConfig) -> Self {
            Self {
                interceptor: AuthInterceptor::new(config),
            }
        }
    }

    impl<S> Layer<S> for AuthLayer {
        type Service = AuthService<S>;

        fn layer(&self, service: S) -> Self::Service {
            AuthService {
                inner: service,
                interceptor: self.interceptor.clone(),
            }
        }
    }

    /// Authentication service wrapper
    #[derive(Clone)]
    pub struct AuthService<S> {
        inner: S,
        interceptor: AuthInterceptor,
    }

    impl<S, B> Service<http::Request<B>> for AuthService<S>
    where
        S: Service<http::Request<B>, Response = http::Response<BoxBody>> + Clone + Send + 'static,
        S::Future: Send + 'static,
        B: Send + 'static,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx)
        }

        fn call(&mut self, request: http::Request<B>) -> Self::Future {
            let interceptor = self.interceptor.clone();
            let mut inner = self.inner.clone();
            let method = request.uri().path().to_string();

            Box::pin(async move {
                // Convert http headers to tonic metadata
                let metadata = request.headers();
                let mut tonic_metadata = MetadataMap::new();

                for (key, value) in metadata.iter() {
                    let key_str = key.as_str();
                    if let Ok(value_str) = value.to_str() {
                        if let Ok(meta_key) =
                            tonic::metadata::MetadataKey::from_bytes(key_str.as_bytes())
                        {
                            if let Ok(meta_value) = value_str.parse() {
                                tonic_metadata.insert(meta_key, meta_value);
                            }
                        }
                    }
                }

                // Authenticate
                match interceptor.authenticate(&tonic_metadata, &method) {
                    Ok(identity) => {
                        // Check authorization
                        if let Err(e) = interceptor.check_authorization(&identity, &method) {
                            error!("Authorization failed for {}: {:?}", method, e);
                            let response = http::Response::builder()
                                .status(http::StatusCode::FORBIDDEN)
                                .body(tonic::body::empty_body())
                                .unwrap();
                            return Ok(response);
                        }

                        // Proceed with request
                        inner.call(request).await
                    }
                    Err(e) => {
                        warn!("Authentication failed for {}: {:?}", method, e);
                        let status_code = match e {
                            AuthError::MissingCredentials
                            | AuthError::InvalidApiKey
                            | AuthError::InvalidToken(_)
                            | AuthError::TokenExpired => http::StatusCode::UNAUTHORIZED,
                            AuthError::InsufficientPermissions { .. } => {
                                http::StatusCode::FORBIDDEN
                            }
                            AuthError::RateLimitExceeded => http::StatusCode::TOO_MANY_REQUESTS,
                            _ => http::StatusCode::INTERNAL_SERVER_ERROR,
                        };

                        let response = http::Response::builder()
                            .status(status_code)
                            .body(tonic::body::empty_body())
                            .unwrap();
                        Ok(response)
                    }
                }
            })
        }
    }
}

/// Extension trait for adding identity to request extensions
pub trait RequestIdentityExt {
    fn identity(&self) -> Option<&Identity>;
    fn set_identity(&mut self, identity: Identity);
}

impl<T> RequestIdentityExt for Request<T> {
    fn identity(&self) -> Option<&Identity> {
        self.extensions().get::<Identity>()
    }

    fn set_identity(&mut self, identity: Identity) {
        self.extensions_mut().insert(identity);
    }
}

// Hex encoding helper (inline to avoid external dependency)
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_key_hashing() {
        let key = "test-api-key-12345";
        let hash = AuthConfig::hash_api_key(key);
        assert_eq!(hash.len(), 64); // SHA256 produces 64 hex chars

        // Same key should produce same hash
        let hash2 = AuthConfig::hash_api_key(key);
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_auth_config_from_env() {
        std::env::set_var("AUTH_ENABLED", "true");
        std::env::set_var("AUTH_API_KEYS", "key1,key2");
        std::env::set_var("AUTH_JWT_SECRET", "test-secret");

        let config = AuthConfig::from_env();

        assert!(config.enabled);
        assert_eq!(config.api_key_hashes.len(), 2);
        assert_eq!(config.jwt_secret, Some("test-secret".to_string()));

        // Cleanup
        std::env::remove_var("AUTH_ENABLED");
        std::env::remove_var("AUTH_API_KEYS");
        std::env::remove_var("AUTH_JWT_SECRET");
    }

    #[test]
    fn test_identity_roles() {
        let mut identity = Identity::anonymous();
        assert!(!identity.has_role("admin"));

        identity.roles = vec!["user".to_string(), "admin".to_string()];
        assert!(identity.has_role("admin"));
        assert!(identity.has_role("user"));
        assert!(identity.has_role("any-role")); // admin has all roles
    }

    #[test]
    fn test_claims_expiration() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        let expired_claims = Claims {
            sub: "test".to_string(),
            iss: "test".to_string(),
            aud: None,
            exp: now - 100,
            iat: now - 200,
            nbf: None,
            jti: None,
            roles: vec![],
            scopes: vec![],
            node_id: None,
            tenant_id: None,
        };
        assert!(expired_claims.is_expired());

        let valid_claims = Claims {
            exp: now + 3600,
            ..expired_claims
        };
        assert!(!valid_claims.is_expired());
    }
}

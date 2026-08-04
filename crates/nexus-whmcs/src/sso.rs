//! Single Sign-On (SSO) for WHMCS integration
//!
//! Provides:
//! - Token-based SSO from WHMCS client area
//! - Session management
//! - Permission validation

use crate::api::WhmcsApi;
use crate::error::{Result, WhmcsError};
use crate::{ClientInfo, ServiceInfo, WhmcsConfig};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// SSO token payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsoToken {
    /// Token ID
    pub id: String,
    /// Client ID
    pub client_id: u64,
    /// Service ID (if accessing specific service)
    pub service_id: Option<u64>,
    /// Server ID (if accessing specific server)
    pub server_id: Option<String>,
    /// Client email
    pub email: String,
    /// Client name
    pub name: String,
    /// Issue time
    pub issued_at: DateTime<Utc>,
    /// Expiration time
    pub expires_at: DateTime<Utc>,
    /// Permissions granted
    pub permissions: Vec<String>,
    /// Source IP (for validation)
    pub source_ip: Option<String>,
}

impl SsoToken {
    /// Check if token is expired
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Check if token has a specific permission
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.contains(&permission.to_string())
            || self.permissions.contains(&"*".to_string())
    }
}

/// SSO session
#[derive(Debug, Clone)]
pub struct SsoSession {
    /// Session ID
    pub id: String,
    /// Associated token
    pub token: SsoToken,
    /// Client info
    pub client: ClientInfo,
    /// Service info (if applicable)
    pub service: Option<ServiceInfo>,
    /// Last activity timestamp
    pub last_activity: DateTime<Utc>,
}

/// SSO manager
pub struct SsoManager {
    config: WhmcsConfig,
    api: Arc<WhmcsApi>,
    /// Active sessions
    sessions: Arc<RwLock<HashMap<String, SsoSession>>>,
    /// Token lifetime in seconds
    token_lifetime_secs: i64,
    /// Session timeout in seconds
    session_timeout_secs: i64,
}

impl SsoManager {
    /// Create a new SSO manager
    pub fn new(config: WhmcsConfig, api: Arc<WhmcsApi>) -> Self {
        Self {
            config,
            api,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            token_lifetime_secs: 300,   // 5 minutes for initial token
            session_timeout_secs: 3600, // 1 hour session
        }
    }

    /// Generate an SSO token for a client
    #[allow(clippy::too_many_arguments)]
    pub fn generate_token(
        &self,
        client_id: u64,
        service_id: Option<u64>,
        server_id: Option<String>,
        email: &str,
        name: &str,
        permissions: Vec<String>,
        source_ip: Option<String>,
    ) -> String {
        let now = Utc::now();
        let token = SsoToken {
            id: Uuid::new_v4().to_string(),
            client_id,
            service_id,
            server_id,
            email: email.to_string(),
            name: name.to_string(),
            issued_at: now,
            expires_at: now + Duration::seconds(self.token_lifetime_secs),
            permissions,
            source_ip,
        };

        // Serialize and sign the token
        let token_json = serde_json::to_string(&token).unwrap();
        let signature = self.sign(&token_json);

        // Encode as base64
        let payload = format!("{}.{}", base64_encode(&token_json), signature);
        payload
    }

    /// Verify and decode an SSO token
    pub fn verify_token(&self, token_str: &str) -> Result<SsoToken> {
        let parts: Vec<&str> = token_str.split('.').collect();
        if parts.len() != 2 {
            return Err(WhmcsError::InvalidSsoToken);
        }

        let token_json = base64_decode(parts[0]).map_err(|_| WhmcsError::InvalidSsoToken)?;
        let signature = parts[1];

        // Verify signature
        let expected_sig = self.sign(&token_json);
        if signature != expected_sig {
            return Err(WhmcsError::InvalidSsoToken);
        }

        // Parse token
        let token: SsoToken =
            serde_json::from_str(&token_json).map_err(|_| WhmcsError::InvalidSsoToken)?;

        // Check expiration
        if token.is_expired() {
            return Err(WhmcsError::SsoTokenExpired);
        }

        Ok(token)
    }

    /// Create a session from a token
    pub async fn create_session(
        &self,
        token: SsoToken,
        source_ip: Option<&str>,
    ) -> Result<SsoSession> {
        // Validate source IP if specified in token
        if let Some(ref token_ip) = token.source_ip {
            if let Some(ip) = source_ip {
                if ip != token_ip {
                    warn!("SSO token IP mismatch: expected {}, got {}", token_ip, ip);
                    return Err(WhmcsError::InvalidSsoToken);
                }
            }
        }

        // Fetch client info
        let client = self.api.get_client(token.client_id).await?;

        // Fetch service info if applicable
        let service = if let Some(service_id) = token.service_id {
            Some(self.api.get_service(service_id).await?)
        } else {
            None
        };

        let session = SsoSession {
            id: Uuid::new_v4().to_string(),
            token,
            client,
            service,
            last_activity: Utc::now(),
        };

        // Store session
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session.id.clone(), session.clone());
        }

        info!(
            "Created SSO session {} for client {}",
            session.id, session.client.id
        );

        Ok(session)
    }

    /// Get a session by ID
    pub async fn get_session(&self, session_id: &str) -> Result<SsoSession> {
        let mut sessions = self.sessions.write().await;

        let session = sessions.get_mut(session_id).ok_or(WhmcsError::InvalidSsoToken)?;

        // Check session timeout
        let timeout = Duration::seconds(self.session_timeout_secs);
        if Utc::now() - session.last_activity > timeout {
            sessions.remove(session_id);
            return Err(WhmcsError::SsoTokenExpired);
        }

        // Update last activity
        session.last_activity = Utc::now();

        Ok(session.clone())
    }

    /// Validate session and check permission
    pub async fn validate_permission(
        &self,
        session_id: &str,
        permission: &str,
    ) -> Result<SsoSession> {
        let session = self.get_session(session_id).await?;

        if !session.token.has_permission(permission) {
            warn!("Session {} lacks permission: {}", session_id, permission);
            return Err(WhmcsError::AuthFailed(format!(
                "Missing permission: {}",
                permission
            )));
        }

        Ok(session)
    }

    /// Validate session and check server access
    pub async fn validate_server_access(
        &self,
        session_id: &str,
        server_id: &str,
    ) -> Result<SsoSession> {
        let session = self.get_session(session_id).await?;

        // Check if token grants access to this server
        if let Some(ref token_server) = session.token.server_id {
            if token_server != server_id {
                return Err(WhmcsError::AuthFailed(
                    "Token does not grant access to this server".to_string(),
                ));
            }
        }

        // Check if service's server_id matches
        if let Some(ref service) = session.service {
            if let Some(ref service_server) = service.server_id {
                if service_server != server_id {
                    return Err(WhmcsError::AuthFailed(
                        "Service does not have access to this server".to_string(),
                    ));
                }
            }
        }

        Ok(session)
    }

    /// End a session
    pub async fn end_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        sessions.remove(session_id);
        info!("Ended SSO session {}", session_id);
        Ok(())
    }

    /// Clean up expired sessions
    pub async fn cleanup_expired_sessions(&self) {
        let timeout = Duration::seconds(self.session_timeout_secs);
        let now = Utc::now();

        let mut sessions = self.sessions.write().await;
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| now - s.last_activity > timeout)
            .map(|(id, _)| id.clone())
            .collect();

        for id in expired {
            sessions.remove(&id);
            debug!("Removed expired session {}", id);
        }
    }

    /// Generate SSO URL for redirecting from WHMCS
    pub fn generate_sso_url(&self, token: &str, return_url: Option<&str>) -> String {
        let mut url = format!(
            "{}/sso?token={}",
            self.config.panel_url,
            urlencoding::encode(token)
        );
        if let Some(ret) = return_url {
            url.push_str(&format!("&return={}", urlencoding::encode(ret)));
        }
        url
    }

    /// Sign data with the webhook secret
    fn sign(&self, data: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.config.webhook_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(data.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

/// Base64 encode
fn base64_encode(data: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(data.as_bytes())
}

/// Base64 decode
fn base64_decode(data: &str) -> std::result::Result<String, ()> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| ())
        .and_then(|bytes| String::from_utf8(bytes).map_err(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_manager() -> SsoManager {
        let config = WhmcsConfig {
            panel_url: "https://panel.example.com".to_string(),
            webhook_secret: "test_secret".to_string(),
            ..Default::default()
        };
        let api = Arc::new(WhmcsApi::new(config.clone()));
        SsoManager::new(config, api)
    }

    #[test]
    fn test_generate_and_verify_token() {
        let manager = create_test_manager();

        let token_str = manager.generate_token(
            123,
            Some(456),
            Some("server-1".to_string()),
            "test@example.com",
            "Test User",
            vec!["read".to_string(), "write".to_string()],
            None,
        );

        let token = manager.verify_token(&token_str).unwrap();

        assert_eq!(token.client_id, 123);
        assert_eq!(token.service_id, Some(456));
        assert_eq!(token.email, "test@example.com");
        assert!(token.has_permission("read"));
        assert!(token.has_permission("write"));
        assert!(!token.has_permission("admin"));
    }

    #[test]
    fn test_invalid_signature() {
        let manager = create_test_manager();

        let token_str = manager.generate_token(
            123,
            None,
            None,
            "test@example.com",
            "Test User",
            vec![],
            None,
        );

        // Tamper with the token
        let tampered = format!("{}x", token_str);
        assert!(manager.verify_token(&tampered).is_err());
    }

    #[test]
    fn test_wildcard_permission() {
        let manager = create_test_manager();

        let token_str = manager.generate_token(
            123,
            None,
            None,
            "test@example.com",
            "Test User",
            vec!["*".to_string()],
            None,
        );

        let token = manager.verify_token(&token_str).unwrap();

        assert!(token.has_permission("read"));
        assert!(token.has_permission("write"));
        assert!(token.has_permission("anything"));
    }

    #[test]
    fn test_generate_sso_url() {
        let manager = create_test_manager();

        let url = manager.generate_sso_url("test_token", Some("/servers/1"));

        assert!(url.contains("https://panel.example.com/sso"));
        assert!(url.contains("token=test_token"));
        assert!(url.contains("return="));
    }
}

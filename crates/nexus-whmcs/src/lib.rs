//! Nexus WHMCS Integration
//!
//! Provides integration with WHMCS for:
//! - Server provisioning and management
//! - Suspend/unsuspend hooks
//! - Usage-based billing
//! - Client portal integration
//! - Single Sign-On (SSO)

pub mod api;
pub mod billing;
pub mod error;
pub mod hooks;
pub mod provisioning;
pub mod sso;

pub use api::WhmcsApi;
pub use billing::{BillingManager, UsageRecord};
pub use error::{Result, WhmcsError};
pub use hooks::{WebhookEvent, WebhookHandler};
pub use provisioning::{ProvisioningModule, ServerConfig};
pub use sso::SsoManager;

use serde::{Deserialize, Serialize};

/// WHMCS module configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhmcsConfig {
    /// WHMCS API URL
    pub api_url: String,
    /// API identifier
    pub api_identifier: String,
    /// API secret
    pub api_secret: String,
    /// Panel URL for SSO redirects
    pub panel_url: String,
    /// Webhook secret for validating incoming webhooks
    pub webhook_secret: String,
    /// Enable usage-based billing
    pub enable_usage_billing: bool,
    /// Billing interval in seconds (default: 3600 = 1 hour)
    pub billing_interval_secs: u64,
}

impl Default for WhmcsConfig {
    fn default() -> Self {
        Self {
            api_url: String::new(),
            api_identifier: String::new(),
            api_secret: String::new(),
            panel_url: String::new(),
            webhook_secret: String::new(),
            enable_usage_billing: false,
            billing_interval_secs: 3600,
        }
    }
}

/// Service/product types supported
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceType {
    /// Game server hosting
    GameServer,
    /// Voice server (TeamSpeak, Mumble)
    VoiceServer,
    /// Web hosting with game panel
    WebHosting,
    /// Dedicated server
    Dedicated,
    /// VPS with game panel
    Vps,
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceType::GameServer => write!(f, "game_server"),
            ServiceType::VoiceServer => write!(f, "voice_server"),
            ServiceType::WebHosting => write!(f, "web_hosting"),
            ServiceType::Dedicated => write!(f, "dedicated"),
            ServiceType::Vps => write!(f, "vps"),
        }
    }
}

/// Service status in WHMCS
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceStatus {
    /// Service is pending setup
    Pending,
    /// Service is active
    Active,
    /// Service is suspended
    Suspended,
    /// Service is terminated
    Terminated,
    /// Service is cancelled
    Cancelled,
    /// Service has fraud flag
    Fraud,
}

impl ServiceStatus {
    /// Convert from WHMCS status string
    pub fn from_whmcs(status: &str) -> Self {
        match status.to_lowercase().as_str() {
            "pending" => ServiceStatus::Pending,
            "active" => ServiceStatus::Active,
            "suspended" => ServiceStatus::Suspended,
            "terminated" => ServiceStatus::Terminated,
            "cancelled" => ServiceStatus::Cancelled,
            "fraud" => ServiceStatus::Fraud,
            _ => ServiceStatus::Pending,
        }
    }

    /// Convert to WHMCS status string
    pub fn to_whmcs(&self) -> &'static str {
        match self {
            ServiceStatus::Pending => "Pending",
            ServiceStatus::Active => "Active",
            ServiceStatus::Suspended => "Suspended",
            ServiceStatus::Terminated => "Terminated",
            ServiceStatus::Cancelled => "Cancelled",
            ServiceStatus::Fraud => "Fraud",
        }
    }
}

/// Client information from WHMCS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// WHMCS client ID
    pub id: u64,
    /// First name
    pub firstname: String,
    /// Last name
    pub lastname: String,
    /// Email address
    pub email: String,
    /// Company name (optional)
    pub company: Option<String>,
    /// Client status
    pub status: String,
}

/// Service/hosting information from WHMCS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// WHMCS service ID
    pub id: u64,
    /// Client ID
    pub client_id: u64,
    /// Product ID
    pub product_id: u64,
    /// Server ID (in Nexus Panel)
    pub server_id: Option<String>,
    /// Domain/hostname
    pub domain: String,
    /// Username
    pub username: String,
    /// Password (for initial setup)
    pub password: Option<String>,
    /// Service status
    pub status: ServiceStatus,
    /// Registration date
    pub regdate: String,
    /// Next due date
    pub nextduedate: String,
    /// Billing cycle
    pub billingcycle: String,
    /// Custom fields
    pub custom_fields: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_status_conversion() {
        assert_eq!(ServiceStatus::from_whmcs("Active"), ServiceStatus::Active);
        assert_eq!(
            ServiceStatus::from_whmcs("SUSPENDED"),
            ServiceStatus::Suspended
        );
        assert_eq!(ServiceStatus::Active.to_whmcs(), "Active");
    }

    #[test]
    fn test_service_type_display() {
        assert_eq!(ServiceType::GameServer.to_string(), "game_server");
    }
}

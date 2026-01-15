//! Error types for WHMCS integration

use thiserror::Error;

/// Result type for WHMCS operations
pub type Result<T> = std::result::Result<T, WhmcsError>;

/// WHMCS integration errors
#[derive(Error, Debug)]
pub enum WhmcsError {
    /// API configuration error
    #[error("WHMCS API not configured: {0}")]
    NotConfigured(String),

    /// Authentication failed
    #[error("WHMCS authentication failed: {0}")]
    AuthFailed(String),

    /// API request failed
    #[error("WHMCS API error: {message}")]
    ApiError {
        message: String,
        status_code: Option<u16>,
    },

    /// Invalid webhook signature
    #[error("Invalid webhook signature")]
    InvalidSignature,

    /// Service not found
    #[error("Service not found: {0}")]
    ServiceNotFound(String),

    /// Client not found
    #[error("Client not found: {0}")]
    ClientNotFound(String),

    /// Provisioning failed
    #[error("Provisioning failed: {0}")]
    ProvisioningFailed(String),

    /// Suspension failed
    #[error("Suspension failed: {0}")]
    SuspensionFailed(String),

    /// Unsuspension failed
    #[error("Unsuspension failed: {0}")]
    UnsuspensionFailed(String),

    /// Termination failed
    #[error("Termination failed: {0}")]
    TerminationFailed(String),

    /// Invalid SSO token
    #[error("Invalid SSO token")]
    InvalidSsoToken,

    /// SSO token expired
    #[error("SSO token expired")]
    SsoTokenExpired,

    /// Billing error
    #[error("Billing error: {0}")]
    BillingError(String),

    /// Network error
    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    /// JSON serialization error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

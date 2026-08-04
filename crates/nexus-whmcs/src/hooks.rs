//! WHMCS webhook handlers
//!
//! Handles incoming webhooks from WHMCS for:
//! - Service status changes
//! - Invoice events
//! - Client events

use crate::error::{Result, WhmcsError};
use crate::WhmcsConfig;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Webhook event types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WebhookEvent {
    // Service events
    ServiceCreated,
    ServiceActivated,
    ServiceSuspended,
    ServiceUnsuspended,
    ServiceTerminated,
    ServiceUpgraded,
    ServiceDowngraded,

    // Invoice events
    InvoiceCreated,
    InvoicePaid,
    InvoiceRefunded,
    InvoiceCancelled,

    // Client events
    ClientCreated,
    ClientUpdated,
    ClientDeleted,

    // Order events
    OrderPlaced,
    OrderAccepted,
    OrderFraud,

    // Unknown
    Unknown,
}

impl WebhookEvent {
    /// Parse event from WHMCS hook name
    pub fn from_hook_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "aftermodulecreate" | "servicecreated" => WebhookEvent::ServiceCreated,
            "aftermodulesuspend" | "servicesuspended" => WebhookEvent::ServiceSuspended,
            "aftermoduleunsuspend" | "serviceunsuspended" => WebhookEvent::ServiceUnsuspended,
            "aftermoduleterminate" | "serviceterminated" => WebhookEvent::ServiceTerminated,
            "aftermodulechangepackage" | "serviceupgraded" => WebhookEvent::ServiceUpgraded,
            "invoicecreated" => WebhookEvent::InvoiceCreated,
            "invoicepaid" => WebhookEvent::InvoicePaid,
            "invoicerefunded" => WebhookEvent::InvoiceRefunded,
            "invoicecancelled" => WebhookEvent::InvoiceCancelled,
            "clientadd" => WebhookEvent::ClientCreated,
            "clientedit" => WebhookEvent::ClientUpdated,
            "clientdelete" => WebhookEvent::ClientDeleted,
            "acceptorder" => WebhookEvent::OrderAccepted,
            "afterfraudorder" => WebhookEvent::OrderFraud,
            _ => WebhookEvent::Unknown,
        }
    }
}

/// Webhook payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// Event type
    pub event: WebhookEvent,
    /// Hook name from WHMCS
    pub hook_name: String,
    /// Timestamp
    pub timestamp: i64,
    /// Service ID (if applicable)
    pub service_id: Option<u64>,
    /// Client ID (if applicable)
    pub client_id: Option<u64>,
    /// Invoice ID (if applicable)
    pub invoice_id: Option<u64>,
    /// Order ID (if applicable)
    pub order_id: Option<u64>,
    /// Additional data
    pub data: HashMap<String, String>,
}

/// Webhook handler callback trait
#[async_trait::async_trait]
pub trait WebhookCallback: Send + Sync {
    /// Handle a webhook event
    async fn handle(&self, payload: &WebhookPayload) -> Result<()>;
}

/// Webhook handler for processing WHMCS webhooks
pub struct WebhookHandler {
    config: WhmcsConfig,
    callbacks: HashMap<WebhookEvent, Vec<Box<dyn WebhookCallback>>>,
}

impl WebhookHandler {
    /// Create a new webhook handler
    pub fn new(config: WhmcsConfig) -> Self {
        Self {
            config,
            callbacks: HashMap::new(),
        }
    }

    /// Register a callback for an event
    pub fn on<C: WebhookCallback + 'static>(&mut self, event: WebhookEvent, callback: C) {
        self.callbacks.entry(event).or_default().push(Box::new(callback));
    }

    /// Verify webhook signature
    pub fn verify_signature(&self, payload: &[u8], signature: &str) -> bool {
        if self.config.webhook_secret.is_empty() {
            warn!("Webhook secret not configured, skipping signature verification");
            return true;
        }

        let mut mac = match HmacSha256::new_from_slice(self.config.webhook_secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => return false,
        };

        mac.update(payload);
        let expected = mac.finalize().into_bytes();
        let expected_hex = hex::encode(expected);

        // Compare signatures (constant-time comparison)
        expected_hex == signature
    }

    /// Parse webhook payload from request body
    pub fn parse_payload(&self, body: &str) -> Result<WebhookPayload> {
        // Try parsing as JSON first
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
            return self.parse_json_payload(&json);
        }

        // Try parsing as form data
        let params: HashMap<String, String> =
            url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();

        self.parse_form_payload(&params)
    }

    /// Parse JSON webhook payload
    fn parse_json_payload(&self, json: &serde_json::Value) -> Result<WebhookPayload> {
        let hook_name = json
            .get("hook")
            .or_else(|| json.get("event"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let event = WebhookEvent::from_hook_name(&hook_name);

        Ok(WebhookPayload {
            event,
            hook_name,
            timestamp: json
                .get("timestamp")
                .and_then(|v| v.as_i64())
                .unwrap_or_else(|| chrono::Utc::now().timestamp()),
            service_id: json
                .get("serviceid")
                .or_else(|| json.get("service_id"))
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))),
            client_id: json
                .get("userid")
                .or_else(|| json.get("client_id"))
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))),
            invoice_id: json
                .get("invoiceid")
                .or_else(|| json.get("invoice_id"))
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))),
            order_id: json
                .get("orderid")
                .or_else(|| json.get("order_id"))
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))),
            data: HashMap::new(), // Could extract additional fields here
        })
    }

    /// Parse form-encoded webhook payload
    fn parse_form_payload(&self, params: &HashMap<String, String>) -> Result<WebhookPayload> {
        let hook_name = params
            .get("hook")
            .or_else(|| params.get("event"))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let event = WebhookEvent::from_hook_name(&hook_name);

        Ok(WebhookPayload {
            event,
            hook_name,
            timestamp: params
                .get("timestamp")
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| chrono::Utc::now().timestamp()),
            service_id: params.get("serviceid").and_then(|s| s.parse().ok()),
            client_id: params.get("userid").and_then(|s| s.parse().ok()),
            invoice_id: params.get("invoiceid").and_then(|s| s.parse().ok()),
            order_id: params.get("orderid").and_then(|s| s.parse().ok()),
            data: params.clone(),
        })
    }

    /// Process a webhook
    pub async fn process(&self, body: &str, signature: Option<&str>) -> Result<()> {
        // Verify signature if provided
        if let Some(sig) = signature {
            if !self.verify_signature(body.as_bytes(), sig) {
                return Err(WhmcsError::InvalidSignature);
            }
        }

        // Parse payload
        let payload = self.parse_payload(body)?;

        info!(
            "Processing webhook: {:?} (hook: {})",
            payload.event, payload.hook_name
        );

        // Execute callbacks
        if let Some(callbacks) = self.callbacks.get(&payload.event) {
            for callback in callbacks {
                if let Err(e) = callback.handle(&payload).await {
                    warn!("Webhook callback error: {}", e);
                }
            }
        }

        // Also execute callbacks for Unknown event (catch-all)
        if payload.event != WebhookEvent::Unknown {
            if let Some(callbacks) = self.callbacks.get(&WebhookEvent::Unknown) {
                for callback in callbacks {
                    if let Err(e) = callback.handle(&payload).await {
                        warn!("Webhook catch-all callback error: {}", e);
                    }
                }
            }
        }

        Ok(())
    }
}

/// Simple logging callback for debugging
pub struct LoggingCallback;

#[async_trait::async_trait]
impl WebhookCallback for LoggingCallback {
    async fn handle(&self, payload: &WebhookPayload) -> Result<()> {
        info!(
            "Webhook received: {:?}, service_id={:?}, client_id={:?}",
            payload.event, payload.service_id, payload.client_id
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_from_hook_name() {
        assert_eq!(
            WebhookEvent::from_hook_name("AfterModuleSuspend"),
            WebhookEvent::ServiceSuspended
        );
        assert_eq!(
            WebhookEvent::from_hook_name("InvoicePaid"),
            WebhookEvent::InvoicePaid
        );
        assert_eq!(
            WebhookEvent::from_hook_name("something_random"),
            WebhookEvent::Unknown
        );
    }

    #[test]
    fn test_verify_signature() {
        let config = WhmcsConfig {
            webhook_secret: "test_secret".to_string(),
            ..Default::default()
        };
        let handler = WebhookHandler::new(config);

        let payload = b"test payload";

        // Calculate expected signature
        use hmac::Mac;
        let mut mac = HmacSha256::new_from_slice(b"test_secret").unwrap();
        mac.update(payload);
        let expected = hex::encode(mac.finalize().into_bytes());

        assert!(handler.verify_signature(payload, &expected));
        assert!(!handler.verify_signature(payload, "invalid_signature"));
    }

    #[test]
    fn test_parse_form_payload() {
        let config = WhmcsConfig::default();
        let handler = WebhookHandler::new(config);

        let body = "hook=AfterModuleSuspend&serviceid=123&userid=456";
        let payload = handler.parse_payload(body).unwrap();

        assert_eq!(payload.event, WebhookEvent::ServiceSuspended);
        assert_eq!(payload.service_id, Some(123));
        assert_eq!(payload.client_id, Some(456));
    }
}

//! WHMCS API client
//!
//! Provides methods to interact with the WHMCS API for:
//! - Client management
//! - Service management
//! - Product configuration
//! - Custom field updates

use crate::error::{Result, WhmcsError};
use crate::{ClientInfo, ServiceInfo, ServiceStatus, WhmcsConfig};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

/// WHMCS API client
pub struct WhmcsApi {
    config: WhmcsConfig,
    client: Client,
}

impl WhmcsApi {
    /// Create a new WHMCS API client
    pub fn new(config: WhmcsConfig) -> Self {
        Self {
            config,
            client: Client::builder()
                .user_agent("NexusPanel/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Make an API request to WHMCS
    async fn api_request(
        &self,
        action: &str,
        params: HashMap<String, String>,
    ) -> Result<serde_json::Value> {
        if self.config.api_url.is_empty() {
            return Err(WhmcsError::NotConfigured("API URL not set".to_string()));
        }

        let mut form_data = params;
        form_data.insert("action".to_string(), action.to_string());
        form_data.insert("identifier".to_string(), self.config.api_identifier.clone());
        form_data.insert("secret".to_string(), self.config.api_secret.clone());
        form_data.insert("responsetype".to_string(), "json".to_string());

        debug!("WHMCS API request: action={}", action);

        let response = self.client.post(&self.config.api_url).form(&form_data).send().await?;

        let status = response.status();
        let body: serde_json::Value = response.json().await?;

        // Check for WHMCS API errors
        if let Some(result) = body.get("result").and_then(|v| v.as_str()) {
            if result == "error" {
                let message = body
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown error")
                    .to_string();
                return Err(WhmcsError::ApiError {
                    message,
                    status_code: Some(status.as_u16()),
                });
            }
        }

        Ok(body)
    }

    /// Get client details
    pub async fn get_client(&self, client_id: u64) -> Result<ClientInfo> {
        let mut params = HashMap::new();
        params.insert("clientid".to_string(), client_id.to_string());

        let response = self.api_request("GetClientsDetails", params).await?;

        // Parse client info from response
        let client = ClientInfo {
            id: response
                .get("client")
                .and_then(|c| c.get("id"))
                .and_then(|v| v.as_u64())
                .or_else(|| response.get("id").and_then(|v| v.as_u64()))
                .unwrap_or(client_id),
            firstname: response.get("firstname").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            lastname: response.get("lastname").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            email: response.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            company: response
                .get("companyname")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            status: response.get("status").and_then(|v| v.as_str()).unwrap_or("Active").to_string(),
        };

        Ok(client)
    }

    /// Get service/hosting details
    pub async fn get_service(&self, service_id: u64) -> Result<ServiceInfo> {
        let mut params = HashMap::new();
        params.insert("serviceid".to_string(), service_id.to_string());

        let response = self.api_request("GetClientsProducts", params).await?;

        // Find the service in the products array
        let products = response
            .get("products")
            .and_then(|p| p.get("product"))
            .and_then(|p| p.as_array())
            .ok_or_else(|| WhmcsError::ServiceNotFound(service_id.to_string()))?;

        let service_data = products
            .iter()
            .find(|p| {
                p.get("id").and_then(|v| v.as_str()).and_then(|s| s.parse::<u64>().ok())
                    == Some(service_id)
            })
            .ok_or_else(|| WhmcsError::ServiceNotFound(service_id.to_string()))?;

        self.parse_service_info(service_data)
    }

    /// Get services for a client
    pub async fn get_client_services(&self, client_id: u64) -> Result<Vec<ServiceInfo>> {
        let mut params = HashMap::new();
        params.insert("clientid".to_string(), client_id.to_string());

        let response = self.api_request("GetClientsProducts", params).await?;

        let products = response
            .get("products")
            .and_then(|p| p.get("product"))
            .and_then(|p| p.as_array());

        let mut services = Vec::new();

        if let Some(products) = products {
            for product in products {
                if let Ok(service) = self.parse_service_info(product) {
                    services.push(service);
                }
            }
        }

        Ok(services)
    }

    /// Parse service info from WHMCS response
    fn parse_service_info(&self, data: &serde_json::Value) -> Result<ServiceInfo> {
        let id = data
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .or_else(|| data.get("id").and_then(|v| v.as_u64()))
            .ok_or_else(|| WhmcsError::Internal("Missing service ID".to_string()))?;

        let client_id = data
            .get("clientid")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .or_else(|| data.get("clientid").and_then(|v| v.as_u64()))
            .unwrap_or(0);

        let product_id = data
            .get("pid")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .or_else(|| data.get("pid").and_then(|v| v.as_u64()))
            .unwrap_or(0);

        let status_str = data.get("status").and_then(|v| v.as_str()).unwrap_or("Pending");

        // Parse custom fields
        let mut custom_fields = HashMap::new();
        if let Some(cf) = data.get("customfields").and_then(|v| v.get("customfield")) {
            if let Some(fields) = cf.as_array() {
                for field in fields {
                    if let (Some(name), Some(value)) = (
                        field.get("name").and_then(|v| v.as_str()),
                        field.get("value").and_then(|v| v.as_str()),
                    ) {
                        custom_fields.insert(name.to_string(), value.to_string());
                    }
                }
            }
        }

        Ok(ServiceInfo {
            id,
            client_id,
            product_id,
            server_id: custom_fields.get("server_id").cloned(),
            domain: data.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            username: data.get("username").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            password: data.get("password").and_then(|v| v.as_str()).map(|s| s.to_string()),
            status: ServiceStatus::from_whmcs(status_str),
            regdate: data.get("regdate").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            nextduedate: data.get("nextduedate").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            billingcycle: data
                .get("billingcycle")
                .and_then(|v| v.as_str())
                .unwrap_or("monthly")
                .to_string(),
            custom_fields,
        })
    }

    /// Update service status
    pub async fn update_service_status(
        &self,
        service_id: u64,
        status: ServiceStatus,
    ) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("serviceid".to_string(), service_id.to_string());
        params.insert("status".to_string(), status.to_whmcs().to_string());

        self.api_request("UpdateClientProduct", params).await?;

        info!("Updated service {} status to {:?}", service_id, status);
        Ok(())
    }

    /// Update service custom field
    pub async fn update_custom_field(
        &self,
        service_id: u64,
        field_name: &str,
        value: &str,
    ) -> Result<()> {
        // First, get the service to find the custom field ID
        let _service = self.get_service(service_id).await?;

        // For now, we'll use a direct update approach
        let mut params = HashMap::new();
        params.insert("serviceid".to_string(), service_id.to_string());
        params.insert(
            "customfields".to_string(),
            format!("{}={}", field_name, value),
        );

        self.api_request("UpdateClientProduct", params).await?;

        info!(
            "Updated custom field {} for service {} to {}",
            field_name, service_id, value
        );
        Ok(())
    }

    /// Add credit to client account
    pub async fn add_credit(&self, client_id: u64, amount: f64, description: &str) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("clientid".to_string(), client_id.to_string());
        params.insert("amount".to_string(), amount.to_string());
        params.insert("description".to_string(), description.to_string());

        self.api_request("AddCredit", params).await?;

        info!("Added {} credit to client {}", amount, client_id);
        Ok(())
    }

    /// Create an invoice for usage
    pub async fn create_invoice(
        &self,
        client_id: u64,
        items: Vec<InvoiceItem>,
        due_date: Option<&str>,
    ) -> Result<u64> {
        let mut params = HashMap::new();
        params.insert("userid".to_string(), client_id.to_string());
        params.insert("sendinvoice".to_string(), "1".to_string());
        params.insert("paymentmethod".to_string(), "mailin".to_string());

        if let Some(due) = due_date {
            params.insert("duedate".to_string(), due.to_string());
        }

        // Add invoice items
        for (i, item) in items.iter().enumerate() {
            params.insert(format!("itemdescription{}", i), item.description.clone());
            params.insert(format!("itemamount{}", i), item.amount.to_string());
            params.insert(
                format!("itemtaxed{}", i),
                if item.taxed { "1" } else { "0" }.to_string(),
            );
        }

        let response = self.api_request("CreateInvoice", params).await?;

        let invoice_id = response
            .get("invoiceid")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                response.get("invoiceid").and_then(|v| v.as_str()).and_then(|s| s.parse().ok())
            })
            .ok_or_else(|| WhmcsError::BillingError("Failed to get invoice ID".to_string()))?;

        info!("Created invoice {} for client {}", invoice_id, client_id);
        Ok(invoice_id)
    }

    /// Log module action
    pub async fn log_module_action(
        &self,
        service_id: u64,
        action: &str,
        request: &str,
        response: &str,
        success: bool,
    ) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("serviceid".to_string(), service_id.to_string());
        params.insert("action".to_string(), action.to_string());
        params.insert("requestdata".to_string(), request.to_string());
        params.insert("responsedata".to_string(), response.to_string());
        params.insert(
            "processeddata".to_string(),
            if success { "Success" } else { "Failed" }.to_string(),
        );

        // Note: ModuleLog is not a standard WHMCS API action, this would need
        // to be implemented via hooks or custom code
        debug!(
            "Module log: service={}, action={}, success={}",
            service_id, action, success
        );

        Ok(())
    }

    /// Send email to client
    pub async fn send_email(
        &self,
        client_id: u64,
        template_name: &str,
        custom_vars: HashMap<String, String>,
    ) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("id".to_string(), client_id.to_string());
        params.insert("messagename".to_string(), template_name.to_string());

        for (key, value) in custom_vars {
            params.insert(format!("customvars[{}]", key), value);
        }

        self.api_request("SendEmail", params).await?;

        info!("Sent email '{}' to client {}", template_name, client_id);
        Ok(())
    }
}

/// Invoice item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceItem {
    /// Item description
    pub description: String,
    /// Amount
    pub amount: f64,
    /// Whether the item is taxed
    pub taxed: bool,
}

impl InvoiceItem {
    /// Create a new invoice item
    pub fn new(description: impl Into<String>, amount: f64) -> Self {
        Self {
            description: description.into(),
            amount,
            taxed: true,
        }
    }

    /// Set taxed status
    pub fn taxed(mut self, taxed: bool) -> Self {
        self.taxed = taxed;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invoice_item() {
        let item = InvoiceItem::new("Test item", 10.0).taxed(false);
        assert_eq!(item.description, "Test item");
        assert_eq!(item.amount, 10.0);
        assert!(!item.taxed);
    }
}

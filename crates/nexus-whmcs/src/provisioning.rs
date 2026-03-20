//! Server provisioning module for WHMCS
//!
//! Handles:
//! - Server creation on order
//! - Suspension/unsuspension
//! - Termination
//! - Password resets
//! - Resource upgrades/downgrades

use crate::api::WhmcsApi;
use crate::error::{Result, WhmcsError};
use crate::{ServiceInfo, ServiceStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info};

/// Server configuration for provisioning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Game type/identifier
    pub game: String,
    /// Server name
    pub name: String,
    /// Memory allocation in MB
    pub memory_mb: u32,
    /// CPU cores/threads
    pub cpu: u32,
    /// Disk space in MB
    pub disk_mb: u32,
    /// Number of player slots
    pub slots: u32,
    /// IP allocation
    pub ip: Option<String>,
    /// Primary port
    pub port: Option<u16>,
    /// Additional ports needed
    pub additional_ports: u32,
    /// Enable backups
    pub backups_enabled: bool,
    /// Backup limit
    pub backup_limit: u32,
    /// Database limit
    pub database_limit: u32,
    /// Startup command override
    pub startup_command: Option<String>,
    /// Custom environment variables
    pub environment: HashMap<String, String>,
    /// Egg/nest ID for Pterodactyl-style configs
    pub egg_id: Option<String>,
    /// Docker image override
    pub docker_image: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            game: String::new(),
            name: String::new(),
            memory_mb: 1024,
            cpu: 100,
            disk_mb: 10240,
            slots: 10,
            ip: None,
            port: None,
            additional_ports: 0,
            backups_enabled: true,
            backup_limit: 2,
            database_limit: 1,
            startup_command: None,
            environment: HashMap::new(),
            egg_id: None,
            docker_image: None,
        }
    }
}

/// Provisioning handler callback
#[async_trait::async_trait]
pub trait ProvisioningHandler: Send + Sync {
    /// Create a new server
    async fn create_server(&self, config: &ServerConfig, service: &ServiceInfo) -> Result<String>;

    /// Suspend a server
    async fn suspend_server(&self, server_id: &str, reason: Option<&str>) -> Result<()>;

    /// Unsuspend a server
    async fn unsuspend_server(&self, server_id: &str) -> Result<()>;

    /// Terminate/delete a server
    async fn terminate_server(&self, server_id: &str) -> Result<()>;

    /// Reset server password
    async fn reset_password(&self, server_id: &str) -> Result<String>;

    /// Update server resources
    async fn update_resources(&self, server_id: &str, config: &ServerConfig) -> Result<()>;

    /// Reinstall server
    async fn reinstall_server(&self, server_id: &str, config: &ServerConfig) -> Result<()>;
}

/// Provisioning module for WHMCS integration
pub struct ProvisioningModule {
    api: Arc<WhmcsApi>,
    handler: Arc<dyn ProvisioningHandler>,
}

impl ProvisioningModule {
    /// Create a new provisioning module
    pub fn new(api: Arc<WhmcsApi>, handler: Arc<dyn ProvisioningHandler>) -> Self {
        Self { api, handler }
    }

    /// Parse server config from WHMCS service custom fields
    pub fn parse_config(service: &ServiceInfo) -> ServerConfig {
        let cf = &service.custom_fields;

        ServerConfig {
            game: cf.get("game").cloned().unwrap_or_default(),
            name: if service.domain.is_empty() {
                format!("Server #{}", service.id)
            } else {
                service.domain.clone()
            },
            memory_mb: cf.get("memory").and_then(|v| v.parse().ok()).unwrap_or(1024),
            cpu: cf.get("cpu").and_then(|v| v.parse().ok()).unwrap_or(100),
            disk_mb: cf.get("disk").and_then(|v| v.parse().ok()).unwrap_or(10240),
            slots: cf.get("slots").and_then(|v| v.parse().ok()).unwrap_or(10),
            ip: cf.get("ip").cloned(),
            port: cf.get("port").and_then(|v| v.parse().ok()),
            additional_ports: cf.get("additional_ports").and_then(|v| v.parse().ok()).unwrap_or(0),
            backups_enabled: cf
                .get("backups_enabled")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(true),
            backup_limit: cf.get("backup_limit").and_then(|v| v.parse().ok()).unwrap_or(2),
            database_limit: cf.get("database_limit").and_then(|v| v.parse().ok()).unwrap_or(1),
            startup_command: cf.get("startup_command").cloned(),
            environment: HashMap::new(), // Would need more complex parsing
            egg_id: cf.get("egg_id").cloned(),
            docker_image: cf.get("docker_image").cloned(),
        }
    }

    /// Create account (provision server on new order)
    pub async fn create_account(&self, service_id: u64) -> Result<ProvisioningResult> {
        info!("Provisioning server for service {}", service_id);

        let service = self.api.get_service(service_id).await?;
        let config = Self::parse_config(&service);

        // Create the server
        let server_id = match self.handler.create_server(&config, &service).await {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to create server for service {}: {}", service_id, e);
                return Err(WhmcsError::ProvisioningFailed(e.to_string()));
            }
        };

        // Update WHMCS with server ID
        self.api.update_custom_field(service_id, "server_id", &server_id).await?;

        // Update service status to Active
        self.api.update_service_status(service_id, ServiceStatus::Active).await?;

        info!(
            "Successfully provisioned server {} for service {}",
            server_id, service_id
        );

        Ok(ProvisioningResult {
            success: true,
            server_id: Some(server_id),
            message: "Server created successfully".to_string(),
        })
    }

    /// Suspend account
    pub async fn suspend_account(
        &self,
        service_id: u64,
        reason: Option<&str>,
    ) -> Result<ProvisioningResult> {
        info!("Suspending server for service {}", service_id);

        let service = self.api.get_service(service_id).await?;
        let server_id = service.server_id.ok_or_else(|| {
            WhmcsError::SuspensionFailed("No server ID found for service".to_string())
        })?;

        // Suspend the server
        self.handler.suspend_server(&server_id, reason).await?;

        // Update WHMCS status
        self.api.update_service_status(service_id, ServiceStatus::Suspended).await?;

        info!(
            "Successfully suspended server {} for service {}",
            server_id, service_id
        );

        Ok(ProvisioningResult {
            success: true,
            server_id: Some(server_id),
            message: "Server suspended successfully".to_string(),
        })
    }

    /// Unsuspend account
    pub async fn unsuspend_account(&self, service_id: u64) -> Result<ProvisioningResult> {
        info!("Unsuspending server for service {}", service_id);

        let service = self.api.get_service(service_id).await?;
        let server_id = service.server_id.ok_or_else(|| {
            WhmcsError::UnsuspensionFailed("No server ID found for service".to_string())
        })?;

        // Unsuspend the server
        self.handler.unsuspend_server(&server_id).await?;

        // Update WHMCS status
        self.api.update_service_status(service_id, ServiceStatus::Active).await?;

        info!(
            "Successfully unsuspended server {} for service {}",
            server_id, service_id
        );

        Ok(ProvisioningResult {
            success: true,
            server_id: Some(server_id),
            message: "Server unsuspended successfully".to_string(),
        })
    }

    /// Terminate account
    pub async fn terminate_account(&self, service_id: u64) -> Result<ProvisioningResult> {
        info!("Terminating server for service {}", service_id);

        let service = self.api.get_service(service_id).await?;
        let server_id = service.server_id.ok_or_else(|| {
            WhmcsError::TerminationFailed("No server ID found for service".to_string())
        })?;

        // Terminate the server
        self.handler.terminate_server(&server_id).await?;

        // Update WHMCS status
        self.api.update_service_status(service_id, ServiceStatus::Terminated).await?;

        // Clear server ID
        self.api.update_custom_field(service_id, "server_id", "").await?;

        info!(
            "Successfully terminated server {} for service {}",
            server_id, service_id
        );

        Ok(ProvisioningResult {
            success: true,
            server_id: Some(server_id),
            message: "Server terminated successfully".to_string(),
        })
    }

    /// Change password
    pub async fn change_password(&self, service_id: u64) -> Result<ProvisioningResult> {
        info!("Resetting password for service {}", service_id);

        let service = self.api.get_service(service_id).await?;
        let server_id = service
            .server_id
            .ok_or_else(|| WhmcsError::Internal("No server ID found for service".to_string()))?;

        // Reset the password
        let new_password = self.handler.reset_password(&server_id).await?;

        info!("Successfully reset password for service {}", service_id);

        Ok(ProvisioningResult {
            success: true,
            server_id: Some(server_id),
            message: format!("Password reset to: {}", new_password),
        })
    }

    /// Change package (upgrade/downgrade)
    pub async fn change_package(&self, service_id: u64) -> Result<ProvisioningResult> {
        info!("Changing package for service {}", service_id);

        let service = self.api.get_service(service_id).await?;
        let config = Self::parse_config(&service);
        let server_id = service
            .server_id
            .ok_or_else(|| WhmcsError::Internal("No server ID found for service".to_string()))?;

        // Update resources
        self.handler.update_resources(&server_id, &config).await?;

        info!(
            "Successfully changed package for server {} (service {})",
            server_id, service_id
        );

        Ok(ProvisioningResult {
            success: true,
            server_id: Some(server_id),
            message: "Package changed successfully".to_string(),
        })
    }

    /// Reinstall server
    pub async fn reinstall(&self, service_id: u64) -> Result<ProvisioningResult> {
        info!("Reinstalling server for service {}", service_id);

        let service = self.api.get_service(service_id).await?;
        let config = Self::parse_config(&service);
        let server_id = service
            .server_id
            .ok_or_else(|| WhmcsError::Internal("No server ID found for service".to_string()))?;

        // Reinstall
        self.handler.reinstall_server(&server_id, &config).await?;

        info!(
            "Successfully reinstalled server {} (service {})",
            server_id, service_id
        );

        Ok(ProvisioningResult {
            success: true,
            server_id: Some(server_id),
            message: "Server reinstalled successfully".to_string(),
        })
    }
}

/// Result of a provisioning operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningResult {
    /// Whether the operation succeeded
    pub success: bool,
    /// Server ID (if applicable)
    pub server_id: Option<String>,
    /// Status message
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config_defaults() {
        let service = ServiceInfo {
            id: 1,
            client_id: 1,
            product_id: 1,
            server_id: None,
            domain: "test-server".to_string(),
            username: "user".to_string(),
            password: None,
            status: ServiceStatus::Pending,
            regdate: "2024-01-01".to_string(),
            nextduedate: "2024-02-01".to_string(),
            billingcycle: "monthly".to_string(),
            custom_fields: HashMap::new(),
        };

        let config = ProvisioningModule::parse_config(&service);

        assert_eq!(config.name, "test-server");
        assert_eq!(config.memory_mb, 1024);
        assert_eq!(config.disk_mb, 10240);
    }

    #[test]
    fn test_parse_config_custom_fields() {
        let mut custom_fields = HashMap::new();
        custom_fields.insert("game".to_string(), "minecraft".to_string());
        custom_fields.insert("memory".to_string(), "2048".to_string());
        custom_fields.insert("slots".to_string(), "20".to_string());

        let service = ServiceInfo {
            id: 1,
            client_id: 1,
            product_id: 1,
            server_id: None,
            domain: "".to_string(),
            username: "user".to_string(),
            password: None,
            status: ServiceStatus::Pending,
            regdate: "2024-01-01".to_string(),
            nextduedate: "2024-02-01".to_string(),
            billingcycle: "monthly".to_string(),
            custom_fields,
        };

        let config = ProvisioningModule::parse_config(&service);

        assert_eq!(config.game, "minecraft");
        assert_eq!(config.memory_mb, 2048);
        assert_eq!(config.slots, 20);
        assert_eq!(config.name, "Server #1");
    }
}

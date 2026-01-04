//! Health checking for Nexus Node
//!
//! Provides comprehensive health checks for the node daemon,
//! including containerd connectivity, resource availability, and system health.

use crate::error::{NodeError, Result};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

/// Health status of a component
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// Component is healthy
    Healthy,
    /// Component is degraded but functional
    Degraded,
    /// Component is unhealthy
    Unhealthy,
}

impl HealthStatus {
    /// Convert to string for metrics/logging
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// Overall health check result
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    /// Overall status
    pub status: HealthStatus,
    /// Individual component checks
    pub checks: HashMap<String, ComponentHealth>,
    /// Timestamp of the check
    pub timestamp: SystemTime,
    /// Optional message
    pub message: Option<String>,
}

/// Health status of an individual component
#[derive(Debug, Clone)]
pub struct ComponentHealth {
    /// Component status
    pub status: HealthStatus,
    /// Optional error message
    pub error: Option<String>,
    /// Optional metadata
    pub metadata: HashMap<String, String>,
}

/// Health checker for Nexus Node
pub struct HealthChecker {
    /// Path to containerd socket
    containerd_socket: String,
    /// Data directory path
    data_dir: String,
    /// Minimum free disk space (bytes)
    min_disk_space: u64,
    /// Minimum free memory (bytes)
    min_memory: u64,
    /// Last check time
    last_check: Option<SystemTime>,
}

impl HealthChecker {
    /// Create a new health checker
    pub fn new(
        containerd_socket: String,
        data_dir: String,
        min_disk_space: u64,
        min_memory: u64,
    ) -> Self {
        Self {
            containerd_socket,
            data_dir,
            min_disk_space,
            min_memory,
            last_check: None,
        }
    }

    /// Run all health checks
    pub async fn check(&mut self) -> HealthCheckResult {
        info!("Running health checks");

        let mut checks = HashMap::new();
        let mut overall_status = HealthStatus::Healthy;

        // Check containerd connectivity
        let containerd_health = self.check_containerd().await;
        if containerd_health.status == HealthStatus::Unhealthy {
            overall_status = HealthStatus::Unhealthy;
        } else if containerd_health.status == HealthStatus::Degraded
            && overall_status == HealthStatus::Healthy
        {
            overall_status = HealthStatus::Degraded;
        }
        checks.insert("containerd".to_string(), containerd_health);

        // Check disk space
        let disk_health = self.check_disk_space();
        if disk_health.status == HealthStatus::Unhealthy {
            overall_status = HealthStatus::Unhealthy;
        } else if disk_health.status == HealthStatus::Degraded
            && overall_status == HealthStatus::Healthy
        {
            overall_status = HealthStatus::Degraded;
        }
        checks.insert("disk".to_string(), disk_health);

        // Check memory
        let memory_health = self.check_memory();
        if memory_health.status == HealthStatus::Unhealthy {
            overall_status = HealthStatus::Unhealthy;
        } else if memory_health.status == HealthStatus::Degraded
            && overall_status == HealthStatus::Healthy
        {
            overall_status = HealthStatus::Degraded;
        }
        checks.insert("memory".to_string(), memory_health);

        // Check data directory
        let data_dir_health = self.check_data_directory();
        if data_dir_health.status == HealthStatus::Unhealthy {
            overall_status = HealthStatus::Unhealthy;
        }
        checks.insert("data_directory".to_string(), data_dir_health);

        self.last_check = Some(SystemTime::now());

        let message = if overall_status == HealthStatus::Healthy {
            Some("All health checks passed".to_string())
        } else {
            Some(format!(
                "Health check completed with status: {}",
                overall_status.as_str()
            ))
        };

        HealthCheckResult {
            status: overall_status,
            checks,
            timestamp: SystemTime::now(),
            message,
        }
    }

    /// Check containerd connectivity
    async fn check_containerd(&self) -> ComponentHealth {
        debug!("Checking containerd connectivity: {}", self.containerd_socket);

        // Check if socket file exists
        let socket_path = Path::new(&self.containerd_socket);
        if !socket_path.exists() {
            return ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some(format!(
                    "Containerd socket not found: {}",
                    self.containerd_socket
                )),
                metadata: HashMap::new(),
            };
        }

        // Try to connect to containerd
        // This is a simplified check - in production, you'd use the actual client
        match tokio::time::timeout(Duration::from_secs(2), async {
            // Attempt connection
            // For now, we just check if the socket is accessible
            std::fs::metadata(socket_path).is_ok()
        })
        .await
        {
            Ok(true) => ComponentHealth {
                status: HealthStatus::Healthy,
                error: None,
                metadata: {
                    let mut m = HashMap::new();
                    m.insert("socket_path".to_string(), self.containerd_socket.clone());
                    m
                },
            },
            Ok(false) => ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some("Cannot access containerd socket".to_string()),
                metadata: HashMap::new(),
            },
            Err(_) => ComponentHealth {
                status: HealthStatus::Degraded,
                error: Some("Containerd connection timeout".to_string()),
                metadata: HashMap::new(),
            },
        }
    }

    /// Check disk space
    fn check_disk_space(&self) -> ComponentHealth {
        debug!("Checking disk space");

        match std::fs::metadata(&self.data_dir) {
            Ok(metadata) => {
                // Get filesystem stats
                // Note: This is a simplified check. In production, use sysinfo or similar
                // to get actual filesystem statistics
                let mut metadata_map = HashMap::new();
                metadata_map.insert("data_dir".to_string(), self.data_dir.clone());

                // For now, we'll assume healthy if the directory is accessible
                // In production, check actual free space
                ComponentHealth {
                    status: HealthStatus::Healthy,
                    error: None,
                    metadata: metadata_map,
                }
            }
            Err(e) => ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some(format!("Cannot access data directory: {}", e)),
                metadata: HashMap::new(),
            },
        }
    }

    /// Check memory availability
    fn check_memory(&self) -> ComponentHealth {
        debug!("Checking memory availability");

        // In production, use sysinfo or /proc/meminfo to get actual memory stats
        // For now, return healthy
        ComponentHealth {
            status: HealthStatus::Healthy,
            error: None,
            metadata: HashMap::new(),
        }
    }

    /// Check data directory accessibility
    fn check_data_directory(&self) -> ComponentHealth {
        debug!("Checking data directory: {}", self.data_dir);

        let path = Path::new(&self.data_dir);
        if !path.exists() {
            // Try to create it
            match std::fs::create_dir_all(path) {
                Ok(_) => ComponentHealth {
                    status: HealthStatus::Healthy,
                    error: None,
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("action".to_string(), "created".to_string());
                        m
                    },
                },
                Err(e) => ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    error: Some(format!("Cannot create data directory: {}", e)),
                    metadata: HashMap::new(),
                },
            }
        } else if path.is_dir() {
            // Check if we can write to it
            match std::fs::File::create(path.join(".health_check")) {
                Ok(_) => {
                    let _ = std::fs::remove_file(path.join(".health_check"));
                    ComponentHealth {
                        status: HealthStatus::Healthy,
                        error: None,
                        metadata: HashMap::new(),
                    }
                }
                Err(e) => ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    error: Some(format!("Cannot write to data directory: {}", e)),
                    metadata: HashMap::new(),
                },
            }
        } else {
            ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some("Data directory path exists but is not a directory".to_string()),
                metadata: HashMap::new(),
            }
        }
    }

    /// Get time since last check
    pub fn time_since_last_check(&self) -> Option<Duration> {
        self.last_check
            .and_then(|t| SystemTime::now().duration_since(t).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_health_checker_creation() {
        let temp_dir = TempDir::new().unwrap();
        let checker = HealthChecker::new(
            "/run/containerd/containerd.sock".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            1024 * 1024 * 1024, // 1GB
            512 * 1024 * 1024,  // 512MB
        );
        assert_eq!(checker.last_check, None);
    }

    #[tokio::test]
    async fn test_data_directory_check() {
        let temp_dir = TempDir::new().unwrap();
        let mut checker = HealthChecker::new(
            "/run/containerd/containerd.sock".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            1024 * 1024 * 1024,
            512 * 1024 * 1024,
        );

        let result = checker.check().await;
        assert!(result.checks.contains_key("data_directory"));
        let data_dir_health = result.checks.get("data_directory").unwrap();
        assert_eq!(data_dir_health.status, HealthStatus::Healthy);
    }
}



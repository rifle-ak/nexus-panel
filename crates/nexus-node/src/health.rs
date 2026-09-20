//! Health checking for Nexus Node.
//!
//! Each check measures something real: containerd answers a version
//! request, the data directory's filesystem has room, the host has memory
//! to spare, the data directory is writable, the firewall is up, and no
//! server is crash-looping. The result is what `/api/v1/node/health`, the
//! gRPC `HealthCheck` and the Analytics page show.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{debug, info};

use crate::container::ContainerManager;
use crate::firewall::Firewall;
use crate::runtime::ContainerRuntime;

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
    /// Free space on the data directory's filesystem below which the node
    /// is unhealthy (bytes).
    min_disk_space: u64,
    /// Available memory below which the node is unhealthy (bytes).
    min_memory: u64,
    /// The runtime, for a real round-trip to containerd.
    runtime: Option<Arc<dyn ContainerRuntime>>,
    /// The firewall, to report when it is off.
    firewall: Option<Arc<Firewall>>,
    /// The manager, to notice servers that keep crashing.
    manager: Option<Arc<ContainerManager>>,
    /// Last check time
    last_check: Option<SystemTime>,
}

/// Crashes within the restart window that count as a crash loop.
const CRASH_LOOP_THRESHOLD: u32 = 3;

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
            runtime: None,
            firewall: None,
            manager: None,
            last_check: None,
        }
    }

    /// Check containerd by asking it, not by looking for its socket.
    pub fn with_runtime(mut self, runtime: Arc<dyn ContainerRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Report the firewall's state.
    pub fn with_firewall(mut self, firewall: Arc<Firewall>) -> Self {
        self.firewall = Some(firewall);
        self
    }

    /// Report servers that keep crashing.
    pub fn with_manager(mut self, manager: Arc<ContainerManager>) -> Self {
        self.manager = Some(manager);
        self
    }

    /// Run all health checks
    pub async fn check(&mut self) -> HealthCheckResult {
        debug!("Running health checks");

        let mut checks = HashMap::new();
        checks.insert("containerd".to_string(), self.check_containerd().await);
        checks.insert("disk".to_string(), self.check_disk_space());
        checks.insert("memory".to_string(), self.check_memory());
        checks.insert("data_directory".to_string(), self.check_data_directory());
        if let Some(fw) = &self.firewall {
            checks.insert("firewall".to_string(), check_firewall(fw));
        }
        if let Some(manager) = &self.manager {
            checks.insert("servers".to_string(), check_servers(manager).await);
        }

        let overall_status =
            checks.values().fold(HealthStatus::Healthy, |acc, c| match (acc, c.status) {
                (HealthStatus::Unhealthy, _) | (_, HealthStatus::Unhealthy) => {
                    HealthStatus::Unhealthy
                }
                (HealthStatus::Degraded, _) | (_, HealthStatus::Degraded) => HealthStatus::Degraded,
                _ => HealthStatus::Healthy,
            });

        self.last_check = Some(SystemTime::now());

        let message = if overall_status == HealthStatus::Healthy {
            Some("All health checks passed".to_string())
        } else {
            let failing: Vec<&str> = checks
                .iter()
                .filter(|(_, c)| c.status != HealthStatus::Healthy)
                .map(|(name, _)| name.as_str())
                .collect();
            info!("Health {}: {}", overall_status.as_str(), failing.join(", "));
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

    /// Check containerd: a version request must come back within a few
    /// seconds. Without a runtime to ask, the socket's presence is all
    /// there is to go on.
    async fn check_containerd(&self) -> ComponentHealth {
        let mut metadata = HashMap::new();
        metadata.insert("socket_path".to_string(), self.containerd_socket.clone());

        let Some(runtime) = &self.runtime else {
            let socket_path = Path::new(&self.containerd_socket);
            return if socket_path.exists() {
                healthy(metadata)
            } else {
                ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    error: Some(format!(
                        "Containerd socket not found: {}",
                        self.containerd_socket
                    )),
                    metadata,
                }
            };
        };

        match tokio::time::timeout(Duration::from_secs(5), runtime.ping()).await {
            Ok(Ok(())) => healthy(metadata),
            Ok(Err(e)) => ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some(format!("Containerd is not answering: {}", e)),
                metadata,
            },
            Err(_) => ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some("Containerd did not answer within 5s".to_string()),
                metadata,
            },
        }
    }

    /// Check free space on the filesystem holding the data directory.
    /// Below the minimum is unhealthy; below twice the minimum is degraded.
    fn check_disk_space(&self) -> ComponentHealth {
        let mut metadata = HashMap::new();
        metadata.insert("data_dir".to_string(), self.data_dir.clone());

        let Some((total, available, mount)) = disk_for_path(Path::new(&self.data_dir)) else {
            return ComponentHealth {
                status: HealthStatus::Degraded,
                error: Some("Could not find the filesystem holding the data directory".to_string()),
                metadata,
            };
        };
        metadata.insert("mount".to_string(), mount);
        metadata.insert("total_bytes".to_string(), total.to_string());
        metadata.insert("available_bytes".to_string(), available.to_string());

        if available < self.min_disk_space {
            ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some(format!(
                    "{} free, below the {} minimum",
                    human_bytes(available),
                    human_bytes(self.min_disk_space)
                )),
                metadata,
            }
        } else if available < self.min_disk_space.saturating_mul(2) {
            ComponentHealth {
                status: HealthStatus::Degraded,
                error: Some(format!(
                    "{} free, getting close to the minimum",
                    human_bytes(available)
                )),
                metadata,
            }
        } else {
            healthy(metadata)
        }
    }

    /// Check the host's available memory against the minimum.
    fn check_memory(&self) -> ComponentHealth {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let available = sys.available_memory();
        let mut metadata = HashMap::new();
        metadata.insert("total_bytes".to_string(), sys.total_memory().to_string());
        metadata.insert("available_bytes".to_string(), available.to_string());
        metadata.insert("swap_used_bytes".to_string(), sys.used_swap().to_string());

        if available < self.min_memory {
            ComponentHealth {
                status: HealthStatus::Unhealthy,
                error: Some(format!(
                    "{} available, below the {} minimum",
                    human_bytes(available),
                    human_bytes(self.min_memory)
                )),
                metadata,
            }
        } else if available < self.min_memory.saturating_mul(2) {
            ComponentHealth {
                status: HealthStatus::Degraded,
                error: Some(format!(
                    "{} available, getting close to the minimum",
                    human_bytes(available)
                )),
                metadata,
            }
        } else {
            healthy(metadata)
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
                    healthy(HashMap::new())
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
        self.last_check.and_then(|t| SystemTime::now().duration_since(t).ok())
    }
}

/// The firewall is expected to be on; off is degraded, with the reason.
fn check_firewall(fw: &Firewall) -> ComponentHealth {
    match fw.disabled_reason() {
        None => healthy(HashMap::new()),
        Some(reason) => ComponentHealth {
            status: HealthStatus::Degraded,
            error: Some(format!("Firewall is off: {}", reason)),
            metadata: HashMap::new(),
        },
    }
}

/// A server that has crashed repeatedly is degraded: something is wrong
/// with it that restarts will not fix, and it may be dragging the node.
async fn check_servers(manager: &ContainerManager) -> ComponentHealth {
    let containers = manager.list_containers().await;
    let mut metadata = HashMap::new();
    metadata.insert("total".to_string(), containers.len().to_string());
    metadata.insert(
        "running".to_string(),
        containers.iter().filter(|c| c.status.is_running()).count().to_string(),
    );
    let looping: Vec<String> = containers
        .iter()
        .filter(|c| c.crash_count >= CRASH_LOOP_THRESHOLD)
        .map(|c| format!("{} ({} crashes)", c.name, c.crash_count))
        .collect();
    if looping.is_empty() {
        healthy(metadata)
    } else {
        ComponentHealth {
            status: HealthStatus::Degraded,
            error: Some(format!("Crash-looping: {}", looping.join(", "))),
            metadata,
        }
    }
}

fn healthy(metadata: HashMap<String, String>) -> ComponentHealth {
    ComponentHealth {
        status: HealthStatus::Healthy,
        error: None,
        metadata,
    }
}

/// The filesystem holding `path`: (total, available, mount point). The
/// disk whose mount point is the longest prefix of the path wins.
fn disk_for_path(path: &Path) -> Option<(u64, u64, String)> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| {
            (
                d.total_space(),
                d.available_space(),
                d.mount_point().to_string_lossy().into_owned(),
            )
        })
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
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

    #[tokio::test]
    async fn disk_and_memory_checks_measure_the_host() {
        let temp_dir = TempDir::new().unwrap();
        // Minimums of zero: healthy, with real numbers attached.
        let mut checker = HealthChecker::new(
            "/nonexistent.sock".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            0,
            0,
        );
        let result = checker.check().await;
        let disk = &result.checks["disk"];
        assert_eq!(disk.status, HealthStatus::Healthy, "{:?}", disk);
        assert!(disk.metadata.contains_key("available_bytes"));
        let memory = &result.checks["memory"];
        assert_eq!(memory.status, HealthStatus::Healthy);
        assert!(memory.metadata["total_bytes"].parse::<u64>().unwrap() > 0);
        // No runtime attached: the missing socket is what is reported.
        assert_eq!(result.checks["containerd"].status, HealthStatus::Unhealthy);

        // Impossible minimums: unhealthy, and the message says by how much.
        let mut checker = HealthChecker::new(
            "/nonexistent.sock".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            u64::MAX / 4,
            u64::MAX / 4,
        );
        let result = checker.check().await;
        assert_eq!(result.checks["disk"].status, HealthStatus::Unhealthy);
        assert_eq!(result.checks["memory"].status, HealthStatus::Unhealthy);
        assert!(result.checks["memory"].error.as_deref().unwrap().contains("below"));
        assert_eq!(result.status, HealthStatus::Unhealthy);
    }

    #[tokio::test]
    async fn runtime_firewall_and_servers_are_checked_when_attached() {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(ContainerManager::new(temp_dir.path().to_path_buf()));
        let firewall = Arc::new(Firewall::disabled("no nft in tests"));
        let mut checker = HealthChecker::new(
            "/nonexistent.sock".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            0,
            0,
        )
        .with_runtime(manager.runtime())
        .with_firewall(firewall)
        .with_manager(manager);
        let result = checker.check().await;
        // The mock runtime answers its ping even though no socket exists.
        assert_eq!(result.checks["containerd"].status, HealthStatus::Healthy);
        let fw = &result.checks["firewall"];
        assert_eq!(fw.status, HealthStatus::Degraded);
        assert!(fw.error.as_deref().unwrap().contains("no nft"));
        assert_eq!(result.checks["servers"].status, HealthStatus::Healthy);
        assert_eq!(result.checks["servers"].metadata["total"], "0");
        assert_eq!(result.status, HealthStatus::Degraded);
    }

    #[test]
    fn bytes_read_well() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}

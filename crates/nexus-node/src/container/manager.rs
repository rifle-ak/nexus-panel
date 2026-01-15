use crate::error::{NodeError, Result};
use crate::container::state::ContainerState;
use crate::metrics::Metrics;
use crate::runtime::{ContainerRuntime, ContainerSpec, Mount, PortMapping, ResourceLimits};
use nexus_config::GameConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Manages container lifecycle and state
pub struct ContainerManager {
    /// Container runtime (Containerd, Docker, or Mock)
    runtime: Arc<dyn ContainerRuntime>,

    /// Container states (by ID)
    states: Arc<RwLock<HashMap<String, ContainerState>>>,

    /// Data directory for server files
    data_dir: PathBuf,

    /// Metrics collector
    metrics: Arc<Metrics>,
}

impl ContainerManager {
    /// Create a new container manager with a specific runtime
    pub fn with_runtime(runtime: Arc<dyn ContainerRuntime>, data_dir: PathBuf) -> Self {
        Self {
            runtime,
            states: Arc::new(RwLock::new(HashMap::new())),
            data_dir,
            metrics: Arc::new(Metrics::default()),
        }
    }

    /// Create a new container manager with metrics
    pub fn with_runtime_and_metrics(
        runtime: Arc<dyn ContainerRuntime>,
        data_dir: PathBuf,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            runtime,
            states: Arc::new(RwLock::new(HashMap::new())),
            data_dir,
            metrics,
        }
    }

    /// Create a new container manager with mock runtime (for testing)
    pub fn new(data_dir: PathBuf) -> Self {
        use crate::runtime::mock::MockRuntime;
        Self::with_runtime(Arc::new(MockRuntime::new()), data_dir)
    }

    /// Get metrics instance
    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.metrics
    }

    /// Create a new container from a Nexus config
    pub async fn create_container(
        &self,
        config: &GameConfig,
        server_id: Option<String>,
    ) -> Result<String> {
        let start = Instant::now();
        let container_id = server_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let server_name = config.metadata.name.clone();

        info!(
            "Creating container {} for server: {}",
            container_id, server_name
        );

        // Check if container already exists
        {
            let states = self.states.read().await;
            if states.contains_key(&container_id) {
                self.metrics.record_container_operation(
                    "create",
                    "already_exists",
                    start.elapsed(),
                );
                return Err(NodeError::ContainerAlreadyExists(container_id));
            }
        }

        // Create server data directory
        let server_dir = self.data_dir.join(&container_id);
        std::fs::create_dir_all(&server_dir).map_err(|e| {
            self.metrics.record_container_operation("create", "error", start.elapsed());
            NodeError::Internal(format!("Failed to create server directory: {}", e))
        })?;

        // Pull image first
        info!("Pulling image: {}", config.container.image);
        let image_pull_start = Instant::now();
        match self.runtime.pull_image(&config.container.image).await {
            Ok(_) => {
                self.metrics
                    .record_image_pull(image_pull_start.elapsed(), true);
            }
            Err(e) => {
                self.metrics
                    .record_image_pull(image_pull_start.elapsed(), false);
                self.metrics.record_container_operation("create", "error", start.elapsed());
                return Err(e);
            }
        }

        // Convert config to container spec
        let spec = Self::config_to_spec(config, &server_dir)?;

        // Create container via runtime
        let _container_info = self.runtime.create(&container_id, spec).await
            .map_err(|e| {
                self.metrics.record_container_operation("create", "error", start.elapsed());
                NodeError::StartFailed {
                    container_id: container_id.clone(),
                    source: e.into(),
                }
            })?;

        // Create state
        let state = ContainerState::new(
            container_id.clone(),
            server_name.clone(),
            config.container.image.clone(),
        );

        // Store state
        {
            let mut states = self.states.write().await;
            states.insert(container_id.clone(), state);
        }

        // Update metrics
        self.metrics.record_container_operation("create", "success", start.elapsed());
        self.update_container_count_metrics().await;

        info!(
            "Container {} created successfully for server: {}",
            container_id, server_name
        );

        Ok(container_id)
    }

    /// Start a container
    pub async fn start_container(&self, container_id: &str) -> Result<()> {
        let start = Instant::now();
        info!("Starting container: {}", container_id);

        // Get current state
        let mut state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| {
                    self.metrics.record_container_operation("start", "not_found", start.elapsed());
                    NodeError::ContainerNotFound(container_id.to_string())
                })?
                .clone()
        };

        // Check if already running
        if state.status.is_running() {
            warn!("Container {} is already running", container_id);
            self.metrics.record_container_operation("start", "already_running", start.elapsed());
            return Ok(());
        }

        // Start container via runtime
        let pid = self.runtime.start(container_id).await
            .map_err(|e| {
                self.metrics.record_container_operation("start", "error", start.elapsed());
                NodeError::StartFailed {
                    container_id: container_id.to_string(),
                    source: e.into(),
                }
            })?;

        state.mark_started(pid);

        // Update state
        {
            let mut states = self.states.write().await;
            states.insert(container_id.to_string(), state);
        }

        // Update metrics
        self.metrics.record_container_operation("start", "success", start.elapsed());
        self.update_container_count_metrics().await;

        info!("Container {} started successfully", container_id);

        Ok(())
    }

    /// Stop a container
    pub async fn stop_container(&self, container_id: &str, timeout: Option<u32>) -> Result<()> {
        let start = Instant::now();
        let timeout = timeout.unwrap_or(30);
        info!("Stopping container: {} (timeout: {}s)", container_id, timeout);

        // Get current state
        let mut state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| {
                    self.metrics.record_container_operation("stop", "not_found", start.elapsed());
                    NodeError::ContainerNotFound(container_id.to_string())
                })?
                .clone()
        };

        // Check if already stopped
        if state.status.is_stopped() {
            warn!("Container {} is already stopped", container_id);
            self.metrics.record_container_operation("stop", "already_stopped", start.elapsed());
            return Ok(());
        }

        // Stop container via runtime (handles graceful shutdown)
        let exit_code = self.runtime.stop(container_id, timeout).await
            .map_err(|e| {
                self.metrics.record_container_operation("stop", "error", start.elapsed());
                NodeError::StopFailed {
                    container_id: container_id.to_string(),
                    source: e.into(),
                }
            })?;

        state.mark_stopped(exit_code);

        // Update state
        {
            let mut states = self.states.write().await;
            states.insert(container_id.to_string(), state);
        }

        // Update metrics
        self.metrics.record_container_operation("stop", "success", start.elapsed());
        self.update_container_count_metrics().await;

        info!("Container {} stopped successfully", container_id);

        Ok(())
    }

    /// Restart a container
    pub async fn restart_container(&self, container_id: &str) -> Result<()> {
        let start = Instant::now();
        info!("Restarting container: {}", container_id);

        self.stop_container(container_id, Some(10)).await?;
        self.start_container(container_id).await?;

        // Increment restart count
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(container_id) {
                state.mark_restarted();
                // Record restart metric
                self.metrics.record_container_restart(
                    &state.id,
                    &state.name,
                );
            }
        }

        self.metrics.record_container_operation("restart", "success", start.elapsed());
        Ok(())
    }

    /// Delete a container
    pub async fn delete_container(&self, container_id: &str, force: bool) -> Result<()> {
        let start = Instant::now();
        info!("Deleting container: {} (force: {})", container_id, force);

        // Get current state
        let state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| {
                    self.metrics.record_container_operation("delete", "not_found", start.elapsed());
                    NodeError::ContainerNotFound(container_id.to_string())
                })?
                .clone()
        };

        // If container is running and not force, return error
        if state.status.is_running() && !force {
            self.metrics.record_container_operation("delete", "running_not_forced", start.elapsed());
            return Err(NodeError::Internal(
                format!("Container {} is still running. Use force=true to stop and delete", container_id)
            ));
        }

        // Stop if running
        if state.status.is_running() {
            self.stop_container(container_id, Some(10)).await?;
        }

        // Delete container via runtime
        self.runtime.delete(container_id).await
            .map_err(|e| {
                self.metrics.record_container_operation("delete", "error", start.elapsed());
                e
            })?;

        // Delete server data directory
        let server_dir = self.data_dir.join(container_id);
        if server_dir.exists() {
            std::fs::remove_dir_all(&server_dir).map_err(|e| NodeError::Internal(
                format!("Failed to remove server directory: {}", e)
            ))?;
        }

        // Remove from state
        {
            let mut states = self.states.write().await;
            states.remove(container_id);
        }

        // Update metrics
        self.metrics.record_container_operation("delete", "success", start.elapsed());
        self.update_container_count_metrics().await;

        info!("Container {} deleted successfully", container_id);

        Ok(())
    }

    /// Get container state
    pub async fn get_state(&self, container_id: &str) -> Result<ContainerState> {
        let states = self.states.read().await;
        states
            .get(container_id)
            .cloned()
            .ok_or_else(|| NodeError::ContainerNotFound(container_id.to_string()))
    }

    /// List all containers
    pub async fn list_containers(&self) -> Vec<ContainerState> {
        let states = self.states.read().await;
        states.values().cloned().collect()
    }

    /// Update container count metrics
    async fn update_container_count_metrics(&self) {
        let containers = self.list_containers().await;
        let total = containers.len();
        let running = containers.iter().filter(|c| c.status.is_running()).count();
        
        let mut by_state = std::collections::HashMap::new();
        for container in &containers {
            let state_str = match container.status {
                crate::container::state::ContainerStatus::Created => "created",
                crate::container::state::ContainerStatus::Running => "running",
                crate::container::state::ContainerStatus::Stopped => "stopped",
                crate::container::state::ContainerStatus::Paused => "paused",
                crate::container::state::ContainerStatus::Failed => "failed",
            };
            *by_state.entry(state_str).or_insert(0) += 1;
        }

        let by_state_vec: Vec<(&str, usize)> = by_state.iter()
            .map(|(k, v)| (*k, *v))
            .collect();

        self.metrics.update_container_counts(total, running, &by_state_vec);
    }

    /// Attach to container console for log streaming (read-only)
    pub async fn attach_console(&self, container_id: &str) -> Result<Box<dyn crate::runtime::ConsoleStream>> {
        // Verify container exists
        {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| NodeError::ContainerNotFound(container_id.to_string()))?;
        }

        // Attach to container console
        self.runtime.attach(container_id).await
    }

    /// Attach to container console (bidirectional - stdin/stdout/stderr)
    pub async fn attach_bidirectional(&self, container_id: &str) -> Result<Box<dyn crate::runtime::BidirectionalConsole>> {
        // Verify container exists and is running
        {
            let states = self.states.read().await;
            let state = states
                .get(container_id)
                .ok_or_else(|| NodeError::ContainerNotFound(container_id.to_string()))?;

            if !state.status.is_running() {
                return Err(NodeError::InvalidInput(format!(
                    "Container {} is not running",
                    container_id
                )));
            }
        }

        // Attach to container console
        self.runtime.attach_bidirectional(container_id).await
    }

    /// Send a command to container stdin (one-shot)
    pub async fn send_command(&self, container_id: &str, command: &str) -> Result<()> {
        // Verify container exists and is running
        {
            let states = self.states.read().await;
            let state = states
                .get(container_id)
                .ok_or_else(|| NodeError::ContainerNotFound(container_id.to_string()))?;

            if !state.status.is_running() {
                return Err(NodeError::InvalidInput(format!(
                    "Container {} is not running",
                    container_id
                )));
            }
        }

        // Send command via runtime
        self.runtime.send_command(container_id, command).await
    }

    /// Convert GameConfig to ContainerSpec
    fn config_to_spec(config: &GameConfig, server_dir: &std::path::Path) -> Result<ContainerSpec> {
        // Build command from startup config
        let mut command = vec![config.startup.command.clone()];
        command.extend(config.startup.args.clone());

        // Convert environment variables
        let mut env = config.container.environment.clone();
        for var in &config.variables {
            env.insert(var.name.clone(), var.default.clone());
        }

        // Create mount for server data
        let mounts = vec![Mount {
            source: server_dir.to_string_lossy().to_string(),
            target: config.startup.working_dir.clone(),
            read_only: false,
        }];

        // Convert ports
        let ports: Vec<PortMapping> = config
            .networking
            .ports
            .iter()
            .filter_map(|port| {
                // Parse internal port (skip templates)
                if let Ok(container_port) = port.internal.parse::<u16>() {
                    let protocol = match port.protocol {
                        nexus_config::Protocol::Tcp => "tcp",
                        nexus_config::Protocol::Udp => "udp",
                        nexus_config::Protocol::Both => "tcp", // Default to TCP for Both
                    };
                    Some(PortMapping {
                        container_port,
                        host_port: container_port, // Use same port for now
                        protocol: protocol.to_string(),
                    })
                } else {
                    None
                }
            })
            .collect();

        // Parse resource limits
        let cpu_shares = config.resources.cpu.shares as u64;
        let memory_bytes = parse_size(&config.resources.memory.max)?;
        let memory_swap_bytes = if let Some(ref swap) = config.resources.memory.swap {
            parse_size(swap)?
        } else {
            0 // No swap limit
        };

        Ok(ContainerSpec {
            image: config.container.image.clone(),
            command: vec![],  // Base command is empty, args contain everything
            args: command,    // Full command + args go here
            env,
            working_dir: config.startup.working_dir.clone(),
            mounts,
            ports,
            resources: ResourceLimits {
                cpu_shares,
                memory_bytes,
                memory_swap_bytes,
            },
        })
    }
}

/// Parse size string (1Gi, 512Mi, etc.) to bytes
fn parse_size(size_str: &str) -> Result<u64> {
    let size_str = size_str.trim();

    if size_str.ends_with("Gi") {
        let num: u64 = size_str.trim_end_matches("Gi").parse()
            .map_err(|_| NodeError::InvalidConfig {
                reason: format!("Invalid size: {}", size_str),
            })?;
        Ok(num * 1024 * 1024 * 1024)
    } else if size_str.ends_with("Mi") {
        let num: u64 = size_str.trim_end_matches("Mi").parse()
            .map_err(|_| NodeError::InvalidConfig {
                reason: format!("Invalid size: {}", size_str),
            })?;
        Ok(num * 1024 * 1024)
    } else {
        // Assume bytes
        size_str.parse().map_err(|_| NodeError::InvalidConfig {
            reason: format!("Invalid size: {}", size_str),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::state::ContainerStatus;
    use tempfile::TempDir;

    fn create_test_config() -> GameConfig {
        GameConfig::from_yaml(r#"
metadata:
  id: test-server
  name: Test Server
  game: minecraft
  version: 1.0.0
  author: test@example.com

container:
  image: "itzg/minecraft-server:latest"
  environment: {}

resources:
  cpu:
    min: 1000
    max: 2000
    shares: 1024
  memory:
    min: 1Gi
    max: 2Gi
    swap: 512Mi
  disk:
    min: 5Gi
    io_priority: normal

startup:
  command: "java"
  args:
    - "-jar"
    - "server.jar"
  working_dir: /home/container
  lifecycle:
    pre_start: []
    post_start: []
    pre_stop: []

networking:
  ports:
    - name: game
      internal: "25565"
      protocol: tcp
      required: true

variables: []

security:
  capabilities:
    add: []
    drop: []
  firewall_rules: []
"#).unwrap()
    }

    #[tokio::test]
    async fn test_create_container() {
        let temp_dir = TempDir::new().unwrap();
        let manager = ContainerManager::new(temp_dir.path().to_path_buf());

        let config = create_test_config();
        let container_id = manager
            .create_container(&config, Some("test-server-1".to_string()))
            .await
            .unwrap();

        assert_eq!(container_id, "test-server-1");

        let state = manager.get_state(&container_id).await.unwrap();
        assert_eq!(state.status, ContainerStatus::Created);
    }

    #[tokio::test]
    async fn test_start_stop_container() {
        let temp_dir = TempDir::new().unwrap();
        let manager = ContainerManager::new(temp_dir.path().to_path_buf());

        let config = create_test_config();
        let container_id = manager
            .create_container(&config, Some("test-server-2".to_string()))
            .await
            .unwrap();

        // Start container
        manager.start_container(&container_id).await.unwrap();

        let state = manager.get_state(&container_id).await.unwrap();
        assert_eq!(state.status, ContainerStatus::Running);
        assert!(state.pid.is_some());

        // Stop container
        manager.stop_container(&container_id, None).await.unwrap();

        let state = manager.get_state(&container_id).await.unwrap();
        assert_eq!(state.status, ContainerStatus::Stopped);
        assert!(state.pid.is_none());
    }

    #[tokio::test]
    async fn test_delete_container() {
        let temp_dir = TempDir::new().unwrap();
        let manager = ContainerManager::new(temp_dir.path().to_path_buf());

        let config = create_test_config();
        let container_id = manager
            .create_container(&config, Some("test-server-3".to_string()))
            .await
            .unwrap();

        // Delete container
        manager.delete_container(&container_id, false).await.unwrap();

        // Should not exist
        let result = manager.get_state(&container_id).await;
        assert!(result.is_err());
    }
}

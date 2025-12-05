use crate::error::{NodeError, Result};
use crate::container::state::{ContainerState, ContainerStatus};
use nexus_config::GameConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Manages container lifecycle and state
pub struct ContainerManager {
    /// Container states (by ID)
    states: Arc<RwLock<HashMap<String, ContainerState>>>,

    /// Data directory for server files
    data_dir: PathBuf,

    /// Containerd socket path (for future use)
    #[allow(dead_code)]
    containerd_socket: String,

    /// Containerd namespace (for future use)
    #[allow(dead_code)]
    namespace: String,
}

impl ContainerManager {
    /// Create a new container manager
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            data_dir,
            containerd_socket: "/run/containerd/containerd.sock".to_string(),
            namespace: "nexus-panel".to_string(),
        }
    }

    /// Configure Containerd connection (for future use)
    pub fn with_containerd(mut self, socket: String, namespace: String) -> Self {
        self.containerd_socket = socket;
        self.namespace = namespace;
        self
    }

    /// Create a new container from a Nexus config
    pub async fn create_container(
        &self,
        config: &GameConfig,
        server_id: Option<String>,
    ) -> Result<String> {
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
                return Err(NodeError::ContainerAlreadyExists(container_id));
            }
        }

        // Create server data directory
        let server_dir = self.data_dir.join(&container_id);
        std::fs::create_dir_all(&server_dir).map_err(|e| NodeError::Internal(
            format!("Failed to create server directory: {}", e)
        ))?;

        // TODO: Create container using Containerd API
        // For now, we'll create the state entry

        let state = ContainerState::new(container_id.clone(), server_name.clone());

        // Store state
        {
            let mut states = self.states.write().await;
            states.insert(container_id.clone(), state);
        }

        info!(
            "Container {} created successfully for server: {}",
            container_id, server_name
        );

        Ok(container_id)
    }

    /// Start a container
    pub async fn start_container(&self, container_id: &str) -> Result<()> {
        info!("Starting container: {}", container_id);

        // Get current state
        let mut state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| NodeError::ContainerNotFound(container_id.to_string()))?
                .clone()
        };

        // Check if already running
        if state.status.is_running() {
            warn!("Container {} is already running", container_id);
            return Ok(());
        }

        // TODO: Start container using Containerd API
        // For now, we'll just update the state

        // Simulate PID assignment
        let pid = std::process::id();
        state.mark_started(pid);

        // Update state
        {
            let mut states = self.states.write().await;
            states.insert(container_id.to_string(), state);
        }

        info!("Container {} started successfully", container_id);

        Ok(())
    }

    /// Stop a container
    pub async fn stop_container(&self, container_id: &str, timeout: Option<u32>) -> Result<()> {
        let timeout = timeout.unwrap_or(30);
        info!("Stopping container: {} (timeout: {}s)", container_id, timeout);

        // Get current state
        let mut state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| NodeError::ContainerNotFound(container_id.to_string()))?
                .clone()
        };

        // Check if already stopped
        if state.status.is_stopped() {
            warn!("Container {} is already stopped", container_id);
            return Ok(());
        }

        // TODO: Stop container using Containerd API with graceful shutdown
        // 1. Send SIGTERM
        // 2. Wait for timeout
        // 3. Send SIGKILL if still running

        // For now, we'll just update the state
        state.mark_stopped(0);

        // Update state
        {
            let mut states = self.states.write().await;
            states.insert(container_id.to_string(), state);
        }

        info!("Container {} stopped successfully", container_id);

        Ok(())
    }

    /// Restart a container
    pub async fn restart_container(&self, container_id: &str) -> Result<()> {
        info!("Restarting container: {}", container_id);

        self.stop_container(container_id, Some(10)).await?;
        self.start_container(container_id).await?;

        // Increment restart count
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(container_id) {
                state.mark_restarted();
            }
        }

        Ok(())
    }

    /// Delete a container
    pub async fn delete_container(&self, container_id: &str, force: bool) -> Result<()> {
        info!("Deleting container: {} (force: {})", container_id, force);

        // Get current state
        let state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| NodeError::ContainerNotFound(container_id.to_string()))?
                .clone()
        };

        // If container is running and not force, return error
        if state.status.is_running() && !force {
            return Err(NodeError::Internal(
                format!("Container {} is still running. Use force=true to stop and delete", container_id)
            ));
        }

        // Stop if running
        if state.status.is_running() {
            self.stop_container(container_id, Some(10)).await?;
        }

        // TODO: Delete container using Containerd API

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_config() -> GameConfig {
        GameConfig::from_yaml(r#"
metadata:
  name: Test Server
  game: minecraft
  version: 1.0.0

container:
  image: "itzg/minecraft-server:latest"
  working_dir: /data

startup:
  command: "java -jar server.jar"

networking:
  ports:
    - container: 25565
      host: 25565
      protocol: tcp

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

use crate::container::state::{ContainerState, ContainerStatus};
use crate::error::{NodeError, Result};
use crate::metrics::Metrics;
use crate::runtime::{
    ContainerInfo, ContainerRuntime, ContainerSpec, Mount, PortMapping, ResourceLimits,
};
use nexus_config::GameConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Subdirectory of `DATA_DIR` where container tracking state is persisted.
/// Kept separate from the per-container data directories so it never appears
/// in the file manager or inside a container's bind mount.
const STATE_SUBDIR: &str = ".nexus/state";

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

    // ── State persistence ────────────────────────────────────────────────

    /// Directory holding per-container persisted state files.
    fn state_dir(&self) -> PathBuf {
        self.data_dir.join(STATE_SUBDIR)
    }

    /// Path of the persisted state file for a container.
    fn state_path(&self, id: &str) -> PathBuf {
        self.state_dir().join(format!("{}.json", id))
    }

    /// Write a container's state to disk (best-effort; failures are logged,
    /// not fatal — an unwritable state file must not break live operations).
    async fn persist_state(&self, state: &ContainerState) {
        let dir = self.state_dir();
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!("Failed to create state dir {:?}: {}", dir, e);
            return;
        }
        match serde_json::to_vec_pretty(state) {
            Ok(bytes) => {
                let path = self.state_path(&state.id);
                if let Err(e) = tokio::fs::write(&path, bytes).await {
                    warn!("Failed to persist state for {}: {}", state.id, e);
                }
            }
            Err(e) => warn!("Failed to serialize state for {}: {}", state.id, e),
        }
    }

    /// Insert or replace a container's in-memory state and persist it to disk.
    async fn set_state(&self, state: ContainerState) {
        self.persist_state(&state).await;
        let mut states = self.states.write().await;
        states.insert(state.id.clone(), state);
    }

    /// Remove a container's in-memory state and its persisted file.
    async fn remove_state(&self, id: &str) {
        {
            let mut states = self.states.write().await;
            states.remove(id);
        }
        let _ = tokio::fs::remove_file(self.state_path(id)).await;
    }

    /// Restore persisted container state from disk and reconcile it against the
    /// runtime's actual view, so the panel reflects reality after a node
    /// restart (containerd keeps running the containers across our restarts).
    ///
    /// Returns the number of containers restored.
    pub async fn restore(&self) -> usize {
        let dir = self.state_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return 0, // No state dir yet — nothing to restore.
        };

        let mut restored = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    warn!("Failed to read state file {:?}: {}", path, e);
                    continue;
                }
            };
            let mut state: ContainerState = match serde_json::from_slice(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    warn!("Skipping unreadable state file {:?}: {}", path, e);
                    continue;
                }
            };

            // Reconcile against what the runtime actually reports.
            match self.runtime.inspect(&state.id).await {
                Ok(info) => reconcile_state(&mut state, &info),
                Err(_) => {
                    // The runtime no longer knows this container, so it cannot
                    // be running. Keep our metadata but clear the running view.
                    if state.status.is_running() {
                        state.status = ContainerStatus::Stopped;
                        state.pid = None;
                    }
                }
            }

            {
                let mut states = self.states.write().await;
                states.insert(state.id.clone(), state.clone());
            }
            self.persist_state(&state).await;
            restored += 1;
        }

        if restored > 0 {
            info!("Restored {} container(s) from disk", restored);
            self.update_container_count_metrics().await;
        }
        restored
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
                self.metrics.record_image_pull(image_pull_start.elapsed(), true);
            }
            Err(e) => {
                self.metrics.record_image_pull(image_pull_start.elapsed(), false);
                self.metrics.record_container_operation("create", "error", start.elapsed());
                return Err(e);
            }
        }

        // Convert config to container spec
        let spec = Self::config_to_spec(config, &server_dir)?;

        // Create container via runtime
        let _container_info = self.runtime.create(&container_id, spec).await.map_err(|e| {
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

        // Store state (in memory + on disk)
        self.set_state(state).await;

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
            self.metrics
                .record_container_operation("start", "already_running", start.elapsed());
            return Ok(());
        }

        // Start container via runtime
        let pid = self.runtime.start(container_id).await.map_err(|e| {
            self.metrics.record_container_operation("start", "error", start.elapsed());
            NodeError::StartFailed {
                container_id: container_id.to_string(),
                source: e.into(),
            }
        })?;

        state.mark_started(pid);

        // Update state (in memory + on disk)
        self.set_state(state).await;

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
        info!(
            "Stopping container: {} (timeout: {}s)",
            container_id, timeout
        );

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
            self.metrics
                .record_container_operation("stop", "already_stopped", start.elapsed());
            return Ok(());
        }

        // Stop container via runtime (handles graceful shutdown)
        let exit_code = self.runtime.stop(container_id, timeout).await.map_err(|e| {
            self.metrics.record_container_operation("stop", "error", start.elapsed());
            NodeError::StopFailed {
                container_id: container_id.to_string(),
                source: e.into(),
            }
        })?;

        state.mark_stopped(exit_code);

        // Update state (in memory + on disk)
        self.set_state(state).await;

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

        // Increment restart count (in memory + on disk)
        if let Ok(mut state) = self.get_state(container_id).await {
            state.mark_restarted();
            self.metrics.record_container_restart(&state.id, &state.name);
            self.set_state(state).await;
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
            self.metrics.record_container_operation(
                "delete",
                "running_not_forced",
                start.elapsed(),
            );
            return Err(NodeError::Internal(format!(
                "Container {} is still running. Use force=true to stop and delete",
                container_id
            )));
        }

        // Stop if running
        if state.status.is_running() {
            self.stop_container(container_id, Some(10)).await?;
        }

        // Delete container via runtime
        self.runtime.delete(container_id).await.map_err(|e| {
            self.metrics.record_container_operation("delete", "error", start.elapsed());
            e
        })?;

        // Delete server data directory
        let server_dir = self.data_dir.join(container_id);
        if server_dir.exists() {
            std::fs::remove_dir_all(&server_dir).map_err(|e| {
                NodeError::Internal(format!("Failed to remove server directory: {}", e))
            })?;
        }

        // Remove from state (in memory + on disk)
        self.remove_state(container_id).await;

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
                crate::container::state::ContainerStatus::Suspended => "suspended",
            };
            *by_state.entry(state_str).or_insert(0) += 1;
        }

        let by_state_vec: Vec<(&str, usize)> = by_state.iter().map(|(k, v)| (*k, *v)).collect();

        self.metrics.update_container_counts(total, running, &by_state_vec);
    }

    /// Attach to container console for log streaming (read-only)
    pub async fn attach_console(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn crate::runtime::ConsoleStream>> {
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
    pub async fn attach_bidirectional(
        &self,
        container_id: &str,
    ) -> Result<Box<dyn crate::runtime::BidirectionalConsole>> {
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

    /// Get the data directory path
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Suspend a container (stop and mark as suspended for billing)
    pub async fn suspend_container(&self, container_id: &str) -> Result<()> {
        let start = Instant::now();
        info!("Suspending container: {}", container_id);

        // Get current state
        let mut state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| {
                    self.metrics.record_container_operation(
                        "suspend",
                        "not_found",
                        start.elapsed(),
                    );
                    NodeError::ContainerNotFound(container_id.to_string())
                })?
                .clone()
        };

        // Stop if running
        if state.status.is_running() {
            self.runtime.stop(container_id, 30).await.map_err(|e| {
                self.metrics.record_container_operation("suspend", "error", start.elapsed());
                NodeError::StopFailed {
                    container_id: container_id.to_string(),
                    source: e.into(),
                }
            })?;
        }

        state.mark_suspended();

        self.set_state(state).await;

        self.metrics.record_container_operation("suspend", "success", start.elapsed());
        self.update_container_count_metrics().await;
        info!("Container {} suspended", container_id);

        Ok(())
    }

    /// Unsuspend a container
    pub async fn unsuspend_container(&self, container_id: &str) -> Result<()> {
        let start = Instant::now();
        info!("Unsuspending container: {}", container_id);

        let mut state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| {
                    self.metrics.record_container_operation(
                        "unsuspend",
                        "not_found",
                        start.elapsed(),
                    );
                    NodeError::ContainerNotFound(container_id.to_string())
                })?
                .clone()
        };

        if !state.status.is_suspended() {
            return Err(NodeError::InvalidInput(format!(
                "Container {} is not suspended",
                container_id
            )));
        }

        state.mark_unsuspended();

        self.set_state(state).await;

        self.metrics.record_container_operation("unsuspend", "success", start.elapsed());
        self.update_container_count_metrics().await;
        info!("Container {} unsuspended", container_id);

        Ok(())
    }

    /// Reinstall a container (wipe data, recreate)
    pub async fn reinstall_container(&self, container_id: &str, preserve_data: bool) -> Result<()> {
        let start = Instant::now();
        info!(
            "Reinstalling container: {} (preserve_data: {})",
            container_id, preserve_data
        );

        // Get the current state to get image info
        let state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| {
                    self.metrics.record_container_operation(
                        "reinstall",
                        "not_found",
                        start.elapsed(),
                    );
                    NodeError::ContainerNotFound(container_id.to_string())
                })?
                .clone()
        };

        // Stop if running
        if state.status.is_running() {
            self.stop_container(container_id, Some(10)).await?;
        }

        // Delete the container from runtime
        let _ = self.runtime.delete(container_id).await;

        // Handle data directory
        let server_dir = self.data_dir.join(container_id);
        if !preserve_data && server_dir.exists() {
            std::fs::remove_dir_all(&server_dir).map_err(|e| {
                NodeError::Internal(format!("Failed to remove server directory: {}", e))
            })?;
        }

        // Recreate the server directory
        std::fs::create_dir_all(&server_dir).map_err(|e| {
            NodeError::Internal(format!("Failed to create server directory: {}", e))
        })?;

        // Re-pull the image
        self.runtime.pull_image(&state.image).await?;

        // Update state to Created
        let new_state = ContainerState::new(
            container_id.to_string(),
            state.name.clone(),
            state.image.clone(),
        );

        self.set_state(new_state).await;

        self.metrics.record_container_operation("reinstall", "success", start.elapsed());
        self.update_container_count_metrics().await;
        info!("Container {} reinstalled", container_id);

        Ok(())
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

    /// Execute a one-shot command as a new process inside a running container
    /// (the shell console), returning its captured output. Unlike
    /// [`send_command`](Self::send_command) — which writes to the game
    /// process's stdin — this spawns a separate process, so it can run
    /// arbitrary tooling (e.g. `npm`, a shell).
    pub async fn exec_command(
        &self,
        container_id: &str,
        command: &[String],
        timeout: std::time::Duration,
    ) -> Result<crate::runtime::ExecOutput> {
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

        self.runtime.exec(container_id, command, timeout).await
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
            command: vec![], // Base command is empty, args contain everything
            args: command,   // Full command + args go here
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

/// Update a persisted [`ContainerState`] to match what the runtime reports.
///
/// A `Suspended` container is a billing marker we set deliberately; the runtime
/// only ever reports it as stopped/created, so we preserve `Suspended` rather
/// than letting reconciliation clear it.
fn reconcile_state(state: &mut ContainerState, info: &ContainerInfo) {
    match info.status.as_str() {
        "running" => {
            state.status = ContainerStatus::Running;
            state.pid = info.pid;
        }
        "paused" | "pausing" => {
            state.status = ContainerStatus::Paused;
            state.pid = info.pid;
        }
        "created" => {
            if !state.status.is_suspended() {
                state.status = ContainerStatus::Created;
            }
            state.pid = None;
        }
        // "stopped", "unknown", or anything else → not running.
        _ => {
            if !state.status.is_suspended() {
                state.status = if info.exit_code.unwrap_or(0) == 0 {
                    ContainerStatus::Stopped
                } else {
                    ContainerStatus::Failed
                };
            }
            state.pid = None;
            if info.exit_code.is_some() {
                state.exit_code = info.exit_code;
            }
        }
    }
}

/// Parse size string (1Gi, 512Mi, etc.) to bytes
fn parse_size(size_str: &str) -> Result<u64> {
    let size_str = size_str.trim();

    if size_str.ends_with("Gi") {
        let num: u64 =
            size_str.trim_end_matches("Gi").parse().map_err(|_| NodeError::InvalidConfig {
                reason: format!("Invalid size: {}", size_str),
            })?;
        Ok(num * 1024 * 1024 * 1024)
    } else if size_str.ends_with("Mi") {
        let num: u64 =
            size_str.trim_end_matches("Mi").parse().map_err(|_| NodeError::InvalidConfig {
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
        GameConfig::from_yaml(
            r#"
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
"#,
        )
        .unwrap()
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

    #[tokio::test]
    async fn test_state_persists_and_restores_across_managers() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Manager A creates a container, then "goes away".
        {
            let manager = ContainerManager::new(data_dir.clone());
            let config = create_test_config();
            manager
                .create_container(&config, Some("persisted-1".to_string()))
                .await
                .unwrap();
        }

        // A fresh manager (as if the node restarted) restores from disk.
        let manager2 = ContainerManager::new(data_dir.clone());
        assert!(manager2.list_containers().await.is_empty());

        let restored = manager2.restore().await;
        assert_eq!(restored, 1);

        let state = manager2.get_state("persisted-1").await.unwrap();
        assert_eq!(state.name, "Test Server");
        // The fresh mock runtime does not know the container, so a previously
        // created (not running) container stays as Created.
        assert_eq!(state.status, ContainerStatus::Created);
    }

    #[tokio::test]
    async fn test_restore_reconciles_stale_running_to_stopped() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Manager A creates and starts a container.
        {
            let manager = ContainerManager::new(data_dir.clone());
            let config = create_test_config();
            let id =
                manager.create_container(&config, Some("running-1".to_string())).await.unwrap();
            manager.start_container(&id).await.unwrap();
            assert!(manager.get_state(&id).await.unwrap().status.is_running());
        }

        // A fresh manager with an empty runtime restores: the container was
        // marked Running on disk but the runtime no longer has it, so
        // reconciliation must clear the running view.
        let manager2 = ContainerManager::new(data_dir.clone());
        assert_eq!(manager2.restore().await, 1);

        let state = manager2.get_state("running-1").await.unwrap();
        assert_eq!(state.status, ContainerStatus::Stopped);
        assert!(state.pid.is_none());
    }

    #[tokio::test]
    async fn test_exec_command_requires_running_and_returns_output() {
        let temp = TempDir::new().unwrap();
        let manager = ContainerManager::new(temp.path().to_path_buf());
        let config = create_test_config();
        let id = manager.create_container(&config, Some("exec-1".to_string())).await.unwrap();

        // Not running yet → rejected.
        assert!(manager
            .exec_command(
                &id,
                &["ls".to_string()],
                crate::runtime::DEFAULT_EXEC_TIMEOUT
            )
            .await
            .is_err());

        // Once running, the mock runtime returns captured output.
        manager.start_container(&id).await.unwrap();
        let out = manager
            .exec_command(
                &id,
                &["echo".to_string(), "hi".to_string()],
                crate::runtime::DEFAULT_EXEC_TIMEOUT,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.contains("echo hi"));
    }

    #[tokio::test]
    async fn test_delete_removes_persisted_state() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let manager = ContainerManager::new(data_dir.clone());

        let config = create_test_config();
        let id = manager.create_container(&config, Some("to-delete".to_string())).await.unwrap();
        assert!(manager.state_path(&id).exists());

        manager.delete_container(&id, true).await.unwrap();
        assert!(!manager.state_path(&id).exists());

        // A restore now finds nothing.
        let manager2 = ContainerManager::new(data_dir);
        assert_eq!(manager2.restore().await, 0);
    }
}

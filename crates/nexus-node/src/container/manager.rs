use crate::container::startup::render_argv;
use crate::container::state::{ContainerState, ContainerStatus};
use crate::error::{NodeError, Result};
use crate::install::{InstallPlan, InstallState};
use crate::metrics::Metrics;
use crate::runtime::{
    container_user, resolve_capabilities, rlimit_name, ContainerInfo, ContainerRuntime,
    ContainerSpec, Mount, PortMapping, ResourceLimits, Rlimit, SeccompProfile, SecurityOptions,
};
use nexus_config::{GameConfig, LifecycleAction};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Subdirectory of `DATA_DIR` where container tracking state is persisted.
/// Kept separate from the per-container data directories so it never appears
/// in the file manager or inside a container's bind mount.
const STATE_SUBDIR: &str = ".nexus/state";

/// Subdirectory of `DATA_DIR` where each container's originating blueprint is
/// kept, so features that need the server's declared configuration (e.g. the
/// game-file update strategy) can recover it after a node restart.
const BLUEPRINT_SUBDIR: &str = ".nexus/blueprints";

/// Memory ceiling for an install container. Generous enough for SteamCMD and
/// an unpack, without reserving the whole game server's allowance for a job
/// that only downloads files.
const INSTALL_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How often an install's output is pulled from its console while it runs.
const INSTALL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// How long a server gets to shut down when neither the caller nor the
/// blueprint says.
const DEFAULT_STOP_TIMEOUT: u32 = 30;

/// Longest a `pre_stop` command's `delay` is honoured; a blueprint cannot
/// make a stop take minutes by asking for a pause.
const MAX_PRE_STOP_DELAY: Duration = Duration::from_secs(60);

/// A watcher re-issues its wait this often; the runtime's wait is otherwise
/// unbounded.
const WATCH_INTERVAL: Duration = Duration::from_secs(3600);

/// A server over its disk allowance is stopped once it has stayed over for
/// this long, so a save that briefly overshoots does not kill the game.
const DISK_GRACE: Duration = Duration::from_secs(60);

/// How often disk usage is measured and console logs are trimmed.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// Console log size at which the log is trimmed, and how much of its tail
/// survives. `NEXUS_CONSOLE_LOG_MAX_BYTES` overrides the first.
const DEFAULT_LOG_MAX_BYTES: u64 = 32 * 1024 * 1024;
const LOG_KEEP_BYTES: u64 = 4 * 1024 * 1024;

/// Manages container lifecycle and state
///
/// Cheap to clone: every field is a shared handle, so background work (the
/// per-server exit watcher, the maintenance loop) holds its own copy.
#[derive(Clone)]
pub struct ContainerManager {
    /// Container runtime (Containerd, Docker, or Mock)
    runtime: Arc<dyn ContainerRuntime>,

    /// Container states (by ID)
    states: Arc<RwLock<HashMap<String, ContainerState>>>,

    /// Data directory for server files
    data_dir: PathBuf,

    /// Metrics collector
    metrics: Arc<Metrics>,

    /// Per-container start generation. Bumped on every start and every
    /// deliberate stop, so an exit watcher can tell "the process I was
    /// watching died" from "someone stopped it, and maybe started it again".
    generations: Arc<RwLock<HashMap<String, u64>>>,

    /// When each container last crashed, for the restart policy's window.
    crashes: Arc<RwLock<HashMap<String, Vec<Instant>>>>,

    /// When each container first went over its disk allowance.
    disk_over_since: Arc<RwLock<HashMap<String, Instant>>>,

    /// The unprivileged user game servers run as and their files belong to.
    game_user: (u32, u32),
}

impl ContainerManager {
    /// Create a new container manager with a specific runtime
    pub fn with_runtime(runtime: Arc<dyn ContainerRuntime>, data_dir: PathBuf) -> Self {
        Self::with_runtime_and_metrics(runtime, data_dir, Arc::new(Metrics::default()))
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
            generations: Arc::new(RwLock::new(HashMap::new())),
            crashes: Arc::new(RwLock::new(HashMap::new())),
            disk_over_since: Arc::new(RwLock::new(HashMap::new())),
            game_user: container_user(),
        }
    }

    /// The uid/gid game servers run as and their files belong to.
    pub fn game_user(&self) -> (u32, u32) {
        self.game_user
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

    // ── Blueprint persistence ────────────────────────────────────────────

    /// Directory holding per-container blueprint copies.
    fn blueprint_dir(&self) -> PathBuf {
        self.data_dir.join(BLUEPRINT_SUBDIR)
    }

    /// Path of the persisted blueprint for a container.
    fn blueprint_path(&self, id: &str) -> PathBuf {
        self.blueprint_dir().join(format!("{}.yaml", id))
    }

    /// Save the blueprint a container was created from (best-effort; a failure
    /// here must not break container creation).
    async fn persist_blueprint(&self, id: &str, config: &GameConfig) {
        let dir = self.blueprint_dir();
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!("Failed to create blueprint dir {:?}: {}", dir, e);
            return;
        }
        match config.to_yaml() {
            Ok(yaml) => {
                let path = self.blueprint_path(id);
                if let Err(e) = tokio::fs::write(&path, yaml).await {
                    warn!("Failed to persist blueprint for {}: {}", id, e);
                }
            }
            Err(e) => warn!("Failed to serialize blueprint for {}: {}", id, e),
        }
    }

    /// Load the blueprint a container was created from, if one was saved.
    ///
    /// Returns `None` when the container predates blueprint persistence or the
    /// stored file is unreadable — callers treat that as "not configured"
    /// rather than an error.
    pub async fn load_blueprint(&self, id: &str) -> Option<GameConfig> {
        let path = self.blueprint_path(id);
        let yaml = tokio::fs::read_to_string(&path).await.ok()?;
        match GameConfig::from_yaml(&yaml) {
            Ok(config) => Some(config),
            Err(e) => {
                warn!("Stored blueprint for {} is unreadable: {}", id, e);
                None
            }
        }
    }

    /// Remove a container's persisted blueprint.
    async fn remove_blueprint(&self, id: &str) {
        let _ = tokio::fs::remove_file(self.blueprint_path(id)).await;
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

            // A server recorded mid-install cannot still be installing: the
            // node that was running it is gone. Mark it failed so the operator
            // can see it and retry, rather than leaving it stuck "Running".
            if state.install_state == InstallState::Running {
                warn!(
                    "Install for {} was interrupted by a node restart; marking it failed",
                    state.id
                );
                state.install_state = InstallState::Failed;
            }

            // A state file written before installs were tracked says nothing
            // about them. Decide from the server's own directory: files on
            // disk mean the game was installed by whatever means, and an empty
            // directory means it still needs installing.
            if state.install_state == InstallState::Unknown {
                state.install_state = if self.server_dir_has_files(&state.id) {
                    InstallState::Installed
                } else {
                    InstallState::Pending
                };
            }

            // Reconcile against what the runtime actually reports.
            match self.runtime.inspect(&state.id).await {
                Ok(info) => reconcile_state(&mut state, &info),
                Err(_) => {
                    // The runtime no longer knows this container, so it cannot
                    // be running. Its files and blueprint are still here, so
                    // rebuild the container record from them rather than
                    // leave a server that can never be started again.
                    if state.status.is_running() {
                        state.status = ContainerStatus::Stopped;
                        state.pid = None;
                    }
                    self.recreate_from_blueprint(&state.id).await;
                }
            }

            if state.disk_limit_bytes == 0 {
                if let Some(config) = self.load_blueprint(&state.id).await {
                    state.disk_limit_bytes = parse_size(&config.resources.disk.min).unwrap_or(0);
                }
            }

            {
                let mut states = self.states.write().await;
                states.insert(state.id.clone(), state.clone());
            }
            self.persist_state(&state).await;

            // A server still running under the restarted node needs someone
            // watching for its exit again.
            if state.status.is_running() {
                let generation = self.bump_generation(&state.id).await;
                self.spawn_exit_watcher(state.id.clone(), generation);
            }
            restored += 1;
        }

        if restored > 0 {
            info!("Restored {} container(s) from disk", restored);
            self.update_container_count_metrics().await;
        }
        restored
    }

    /// Rebuild the runtime's record of a container from its stored blueprint.
    ///
    /// Used when containerd has lost the container (a namespace wipe, a
    /// reinstall of containerd) but the server's files and blueprint survive.
    async fn recreate_from_blueprint(&self, id: &str) {
        let Some(config) = self.load_blueprint(id).await else {
            warn!(
                "Container {} is gone from the runtime and has no stored blueprint; it cannot be started until it is recreated",
                id
            );
            return;
        };
        let server_dir = self.data_dir.join(id);
        let spec = match Self::config_to_spec(&config, &server_dir, self.game_user) {
            Ok(spec) => spec,
            Err(e) => {
                warn!("Cannot rebuild container {} from its blueprint: {}", id, e);
                return;
            }
        };
        if let Err(e) = self.runtime.pull_image(&config.container.image).await {
            warn!("Cannot rebuild container {}: image pull failed: {}", id, e);
            return;
        }
        match self.runtime.create(id, spec).await {
            Ok(_) => info!("Rebuilt container {} from its stored blueprint", id),
            Err(e) => warn!("Cannot rebuild container {}: {}", id, e),
        }
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

        // Create server data directory, owned by the user the game runs as.
        let server_dir = self.data_dir.join(&container_id);
        std::fs::create_dir_all(&server_dir).map_err(|e| {
            self.metrics.record_container_operation("create", "error", start.elapsed());
            NodeError::Internal(format!("Failed to create server directory: {}", e))
        })?;
        chown_path(&server_dir, self.game_user);

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
        let spec = Self::config_to_spec(config, &server_dir, self.game_user)?;

        // Create container via runtime
        let _container_info = self.runtime.create(&container_id, spec).await.map_err(|e| {
            self.metrics.record_container_operation("create", "error", start.elapsed());
            NodeError::StartFailed {
                container_id: container_id.clone(),
                source: e.into(),
            }
        })?;

        // Create state. A blueprint that declares no install has nothing to
        // fetch, so its server is ready to start immediately; anything else
        // stays un-startable until its game files are actually there.
        let mut state = ContainerState::new(
            container_id.clone(),
            server_name.clone(),
            config.container.image.clone(),
        );
        state.install_state = if crate::install::plan(config).is_some() {
            InstallState::Pending
        } else {
            InstallState::NotRequired
        };
        state.disk_limit_bytes = parse_size(&config.resources.disk.min).unwrap_or(0);

        // Store state (in memory + on disk)
        self.set_state(state).await;

        // Keep the originating blueprint so per-server features (e.g. the
        // game-file update strategy) survive a node restart.
        self.persist_blueprint(&container_id, config).await;

        // Update metrics
        self.metrics.record_container_operation("create", "success", start.elapsed());
        self.update_container_count_metrics().await;

        info!(
            "Container {} created successfully for server: {}",
            container_id, server_name
        );

        Ok(container_id)
    }

    // ── Game-file installation ───────────────────────────────────────────

    /// Whether a server's data directory holds anything at all.
    ///
    /// Used to classify servers created before install state was tracked.
    fn server_dir_has_files(&self, container_id: &str) -> bool {
        std::fs::read_dir(self.data_dir.join(container_id))
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    }

    /// Container id used for a server's one-shot install container.
    ///
    /// Derived from the server's id so a crashed node can find and clean up a
    /// leftover install container on the next attempt.
    fn install_container_id(container_id: &str) -> String {
        format!("{}-install", container_id)
    }

    /// The install this server's blueprint asks for, if any.
    pub async fn install_plan(&self, container_id: &str) -> Option<InstallPlan> {
        crate::install::plan(&self.load_blueprint(container_id).await?)
    }

    /// Record a server's install state (in memory and on disk).
    pub async fn set_install_state(&self, container_id: &str, install: InstallState) {
        let mut state = {
            let states = self.states.read().await;
            match states.get(container_id) {
                Some(state) => state.clone(),
                None => return,
            }
        };
        state.install_state = install;
        self.set_state(state).await;
    }

    /// Install a server's game files, blocking until the install finishes.
    ///
    /// The install runs as its own short-lived container: the blueprint's
    /// script, in the install image, with the server's data directory mounted
    /// where the script expects it. Output is streamed into `sink` as it
    /// arrives, so a caller can show progress on an install that runs for
    /// half an hour.
    ///
    /// Returns the script's exit code. A non-zero code is a failed install,
    /// not an error: the caller reports it with the log that explains it.
    pub async fn run_install<F>(&self, container_id: &str, mut sink: F) -> Result<i32>
    where
        F: FnMut(String) + Send,
    {
        let plan = self.install_plan(container_id).await.ok_or_else(|| {
            NodeError::InvalidInput(format!(
                "server {} has no blueprint on file to install from",
                container_id
            ))
        })?;

        // The server's own data directory is what the install populates.
        let server_dir = self.data_dir.join(container_id);
        std::fs::create_dir_all(&server_dir).map_err(|e| {
            NodeError::Internal(format!("Failed to create server directory: {}", e))
        })?;

        self.set_install_state(container_id, InstallState::Running).await;

        let result = self.run_install_container(container_id, &plan, &server_dir, &mut sink).await;

        let state = match &result {
            Ok(0) => InstallState::Installed,
            _ => InstallState::Failed,
        };
        self.set_install_state(container_id, state).await;

        result
    }

    /// The container half of [`run_install`]: create, run, drain, clean up.
    async fn run_install_container<F>(
        &self,
        container_id: &str,
        plan: &InstallPlan,
        server_dir: &Path,
        sink: &mut F,
    ) -> Result<i32>
    where
        F: FnMut(String) + Send,
    {
        let install_id = Self::install_container_id(container_id);

        // A leftover from a previous attempt (or a node that died mid-install)
        // would make create fail with "already exists".
        let _ = self.runtime.delete(&install_id).await;

        self.runtime.pull_image(&plan.image).await?;

        let spec = ContainerSpec {
            image: plan.image.clone(),
            command: vec![],
            args: plan.argv(),
            env: plan.env.clone(),
            working_dir: plan.server_dir.clone(),
            mounts: vec![Mount {
                source: server_dir.to_string_lossy().to_string(),
                target: plan.server_dir.clone(),
                read_only: false,
            }],
            ports: vec![],
            // An install is a download and an unpack: it wants I/O and disk,
            // not the memory headroom the game itself will need.
            resources: ResourceLimits {
                cpu_shares: 1024,
                cpu_millicores: None,
                memory_bytes: INSTALL_MEMORY_BYTES,
                memory_swap_bytes: 0,
                pids_limit: Some(2048),
                rlimits: Vec::new(),
                // SteamCMD raises its own descriptor limit to 2048 and warns
                // loudly when it cannot.
                nofile: crate::runtime::DEFAULT_NOFILE,
            },
            // The script lays files down as root; they are handed to the
            // game's user once it has finished.
            security: SecurityOptions::for_install(),
            hostname: format!("install-{}", short_id(container_id)),
        };

        self.runtime.create(&install_id, spec).await?;
        info!(
            "Installing game files for {} using {} ({})",
            container_id, plan.image, plan.server_dir
        );

        let start_result = self.runtime.start(&install_id).await;
        if let Err(e) = start_result {
            let _ = self.runtime.delete(&install_id).await;
            return Err(e);
        }

        let exit_code = self.drain_until_exit(&install_id, plan.timeout, sink).await;

        // Whatever happened, do not leave the install container (or its
        // rootfs snapshot) behind.
        if exit_code.is_err() {
            let _ = self.runtime.stop(&install_id, 5).await;
        }
        let _ = self.runtime.delete(&install_id).await;

        // Whatever the script wrote belongs to the game now.
        let dir = server_dir.to_path_buf();
        let user = self.game_user;
        let _ = tokio::task::spawn_blocking(move || chown_recursive(&dir, user)).await;

        exit_code
    }

    /// Follow an install container's output until its process exits.
    async fn drain_until_exit<F>(
        &self,
        install_id: &str,
        timeout: std::time::Duration,
        sink: &mut F,
    ) -> Result<i32>
    where
        F: FnMut(String) + Send,
    {
        let mut console = self.runtime.attach(install_id).await.ok();

        let wait = self.runtime.wait(install_id, timeout);
        tokio::pin!(wait);

        loop {
            tokio::select! {
                exit = &mut wait => {
                    // Pick up whatever the script printed just before exiting.
                    drain_console(&mut console, sink).await;
                    return exit;
                }
                _ = tokio::time::sleep(INSTALL_POLL_INTERVAL) => {
                    drain_console(&mut console, sink).await;
                }
            }
        }
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

        // A suspended server is one the billing system said may not run.
        if state.status.is_suspended() {
            self.metrics.record_container_operation("start", "suspended", start.elapsed());
            return Err(NodeError::InvalidInput(format!(
                "Container {} is suspended",
                container_id
            )));
        }

        // A server whose game files were never installed cannot start: its
        // startup command does not exist yet. Saying so beats letting runc
        // report a missing binary.
        if let Some(reason) = state.install_state.blocked_reason() {
            self.metrics
                .record_container_operation("start", "not_installed", start.elapsed());
            return Err(NodeError::InvalidInput(reason.to_string()));
        }

        // Over its disk allowance: refuse rather than let it write more.
        if state.disk_exceeded() {
            self.metrics
                .record_container_operation("start", "disk_exceeded", start.elapsed());
            return Err(NodeError::InvalidInput(format!(
                "Container {} is over its disk allowance ({} of {} MiB used); free space before starting it",
                container_id,
                state.disk_used_bytes / (1024 * 1024),
                state.disk_limit_bytes / (1024 * 1024)
            )));
        }

        // Files the panel or an install wrote as root must be the game's
        // before it runs as its own user.
        self.ensure_ownership(container_id).await;

        // Any watcher from a previous run is now stale.
        let generation = self.bump_generation(container_id).await;

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

        // Watch for the process exiting on its own.
        self.spawn_exit_watcher(container_id.to_string(), generation);

        // Update metrics
        self.metrics.record_container_operation("start", "success", start.elapsed());
        self.update_container_count_metrics().await;

        info!("Container {} started successfully", container_id);

        Ok(())
    }

    /// Stop a container.
    ///
    /// The shutdown is as graceful as the blueprint allows: `pre_stop`
    /// console commands first (`save-all`, `stop`), or the blueprint's
    /// `stop_signal`, and only then the runtime's SIGTERM/SIGKILL. A game
    /// that is told to stop saves its world; one that is killed loses it.
    pub async fn stop_container(&self, container_id: &str, timeout: Option<u32>) -> Result<()> {
        let start = Instant::now();
        let blueprint = self.load_blueprint(container_id).await;
        let timeout = timeout
            .or_else(|| {
                blueprint
                    .as_ref()
                    .and_then(|c| c.startup.stop_timeout.as_deref())
                    .and_then(nexus_config::parse_duration)
                    .map(|d| d.as_secs() as u32)
            })
            .unwrap_or(DEFAULT_STOP_TIMEOUT);
        info!(
            "Stopping container: {} (timeout: {}s)",
            container_id, timeout
        );

        // This stop is deliberate: the exit watcher must not treat the exit
        // it is about to see as a crash.
        self.bump_generation(container_id).await;

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

        // Ask nicely the way the blueprint says, then let the runtime
        // finish the job (or reap the task if the ask worked).
        let remaining = if state.status.is_running() {
            self.graceful_shutdown(container_id, blueprint.as_ref(), timeout).await
        } else {
            timeout
        };
        let exit_code = self.runtime.stop(container_id, remaining).await.map_err(|e| {
            self.metrics.record_container_operation("stop", "error", start.elapsed());
            NodeError::StopFailed {
                container_id: container_id.to_string(),
                source: e.into(),
            }
        })?;

        // A stop that was asked for is not a crash, whatever the exit code:
        // many games exit non-zero on a console `stop`.
        state.mark_stopped(exit_code);
        state.status = ContainerStatus::Stopped;
        self.disk_over_since.write().await.remove(container_id);

        // Update state (in memory + on disk)
        self.set_state(state).await;

        // Update metrics
        self.metrics.record_container_operation("stop", "success", start.elapsed());
        self.update_container_count_metrics().await;

        info!("Container {} stopped successfully", container_id);

        Ok(())
    }

    /// Run the blueprint's shutdown sequence and wait for the process to
    /// exit. Returns how much of `timeout` is left for the runtime's own stop.
    async fn graceful_shutdown(
        &self,
        container_id: &str,
        blueprint: Option<&GameConfig>,
        timeout: u32,
    ) -> u32 {
        let Some(config) = blueprint else {
            return timeout;
        };
        let started = Instant::now();
        let budget = Duration::from_secs(timeout as u64);

        let commands: Vec<&LifecycleAction> = config
            .startup
            .lifecycle
            .as_ref()
            .map(|l| l.pre_stop.iter().collect())
            .unwrap_or_default();

        let mut asked = false;
        for action in commands {
            if let LifecycleAction::SendCommand { command, delay } = action {
                match self.runtime.send_command(container_id, command).await {
                    Ok(()) => asked = true,
                    Err(e) => {
                        warn!(
                            "pre_stop command {:?} for {} could not be sent: {}",
                            command, container_id, e
                        );
                        break;
                    }
                }
                if let Some(delay) = delay.as_deref().and_then(nexus_config::parse_duration) {
                    tokio::time::sleep(delay.min(MAX_PRE_STOP_DELAY)).await;
                }
            }
        }

        if !asked {
            if let Some(signal) = config.startup.stop_signal.as_deref().and_then(signal_number) {
                if signal != libc::SIGTERM {
                    match self.runtime.kill(container_id, signal).await {
                        Ok(()) => asked = true,
                        Err(e) => warn!(
                            "stop signal {} for {} could not be sent: {}",
                            signal, container_id, e
                        ),
                    }
                }
            }
        }

        if !asked {
            return timeout;
        }

        let left = budget.saturating_sub(started.elapsed());
        match self.runtime.wait(container_id, left).await {
            Ok(_) => {
                info!("Container {} shut down cleanly", container_id);
                // Just enough for the runtime to reap the exited task.
                5
            }
            Err(_) => {
                warn!(
                    "Container {} ignored its shutdown request for {}s; forcing",
                    container_id,
                    left.as_secs()
                );
                // The graceful budget is spent; a short SIGTERM grace remains.
                5
            }
        }
    }

    // ── Exit watching and crash recovery ─────────────────────────────

    async fn bump_generation(&self, container_id: &str) -> u64 {
        let mut generations = self.generations.write().await;
        let next = generations.get(container_id).copied().unwrap_or(0) + 1;
        generations.insert(container_id.to_string(), next);
        next
    }

    async fn current_generation(&self, container_id: &str) -> u64 {
        self.generations.read().await.get(container_id).copied().unwrap_or(0)
    }

    /// Follow a started container until its process exits, then record what
    /// happened and apply the blueprint's restart policy.
    fn spawn_exit_watcher(&self, container_id: String, generation: u64) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                match this.runtime.wait(&container_id, WATCH_INTERVAL).await {
                    Ok(code) => {
                        this.handle_exit(&container_id, generation, code).await;
                        return;
                    }
                    Err(_) => {
                        // The wait timed out or the runtime hiccuped. Stop
                        // watching if this run is over; otherwise look at the
                        // runtime's view before waiting again.
                        if this.current_generation(&container_id).await != generation {
                            return;
                        }
                        match this.runtime.inspect(&container_id).await {
                            Ok(info) if info.status == "running" => continue,
                            Ok(info) => {
                                let code = info.exit_code.unwrap_or(-1);
                                this.handle_exit(&container_id, generation, code).await;
                                return;
                            }
                            Err(_) => {
                                this.handle_exit(&container_id, generation, -1).await;
                                return;
                            }
                        }
                    }
                }
            }
        });
    }

    /// The process behind `container_id` exited without being asked to.
    async fn handle_exit(&self, container_id: &str, generation: u64, exit_code: i32) {
        {
            // A deliberate stop (or a newer start) already moved on.
            let mut generations = self.generations.write().await;
            if generations.get(container_id).copied().unwrap_or(0) != generation {
                return;
            }
            generations.insert(container_id.to_string(), generation + 1);
        }

        let mut state = {
            let states = self.states.read().await;
            match states.get(container_id) {
                Some(state) if state.status.is_running() => state.clone(),
                _ => return,
            }
        };

        let crashed = exit_code != 0;
        state.mark_stopped(exit_code);
        if crashed {
            state.crash_count += 1;
            self.metrics.record_container_operation("crash", "detected", Duration::ZERO);
            warn!(
                "Container {} ({}) exited unexpectedly with code {} (crash #{})",
                container_id, state.name, exit_code, state.crash_count
            );
        } else {
            info!(
                "Container {} ({}) exited cleanly on its own",
                container_id, state.name
            );
        }
        let name = state.name.clone();
        self.set_state(state).await;
        self.update_container_count_metrics().await;

        // Reap the exited task so the next start does not collide with it.
        let _ = self.runtime.stop(container_id, 1).await;

        if !crashed {
            return;
        }

        let Some(config) = self.load_blueprint(container_id).await else {
            return;
        };
        let policy = &config.startup.restart;
        if !policy.on_crash {
            return;
        }
        let reset_after =
            nexus_config::parse_duration(&policy.reset_after).unwrap_or(Duration::from_secs(600));
        let delay = nexus_config::parse_duration(&policy.delay).unwrap_or(Duration::from_secs(5));

        let recent = {
            let mut crashes = self.crashes.write().await;
            let entry = crashes.entry(container_id.to_string()).or_default();
            let now = Instant::now();
            entry.retain(|t| now.duration_since(*t) < reset_after);
            entry.push(now);
            entry.len() as u32
        };
        if recent > policy.max_retries {
            error!(
                "Container {} ({}) crashed {} times within {}; not restarting it again",
                container_id, name, recent, policy.reset_after
            );
            return;
        }

        info!(
            "Restarting container {} ({}) in {}s (crash {} of {} allowed)",
            container_id,
            name,
            delay.as_secs(),
            recent,
            policy.max_retries
        );
        let this = self.clone();
        let id = container_id.to_string();
        let expected = generation + 1;
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // Someone may have stopped or started it by hand meanwhile.
            if this.current_generation(&id).await != expected {
                return;
            }
            if let Err(e) = this.start_container(&id).await {
                error!("Crash restart of container {} failed: {}", id, e);
            } else {
                this.metrics.record_container_operation("crash", "restarted", Duration::ZERO);
            }
        });
    }

    // ── Ownership ────────────────────────────────────────────────────

    /// Make sure a server's directory belongs to the game's user.
    ///
    /// Cheap when it already does (one `stat`); a full walk only when the
    /// top level is wrong, which is the case for servers created before
    /// games ran unprivileged.
    async fn ensure_ownership(&self, container_id: &str) {
        let dir = self.data_dir.join(container_id);
        let user = self.game_user;
        let _ = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;
            match std::fs::metadata(&dir) {
                Ok(meta) if meta.uid() == user.0 && meta.gid() == user.1 => {}
                Ok(_) => chown_recursive(&dir, user),
                Err(_) => {}
            }
        })
        .await;
    }

    // ── Disk usage and log housekeeping ──────────────────────────────

    /// Measure every server's directory and stop any that has stayed over
    /// its allowance.
    pub async fn refresh_disk_usage(&self) {
        let ids: Vec<String> = self.states.read().await.keys().cloned().collect();
        for id in ids {
            let dir = self.data_dir.join(&id);
            let used = tokio::task::spawn_blocking(move || crate::provision::dir_size(&dir))
                .await
                .unwrap_or(0);

            let (exceeded, running, name) = {
                let mut states = self.states.write().await;
                let Some(state) = states.get_mut(&id) else {
                    continue;
                };
                state.disk_used_bytes = used;
                (
                    state.disk_exceeded(),
                    state.status.is_running(),
                    state.name.clone(),
                )
            };

            if !exceeded {
                self.disk_over_since.write().await.remove(&id);
                continue;
            }
            if !running {
                continue;
            }
            let since = {
                let mut over = self.disk_over_since.write().await;
                *over.entry(id.clone()).or_insert_with(Instant::now)
            };
            if since.elapsed() >= DISK_GRACE {
                error!(
                    "Container {} ({}) is over its disk allowance ({} MiB used); stopping it",
                    id,
                    name,
                    used / (1024 * 1024)
                );
                if let Err(e) = self.stop_container(&id, None).await {
                    warn!("Could not stop over-quota container {}: {}", id, e);
                }
            } else {
                warn!(
                    "Container {} ({}) is over its disk allowance ({} MiB used); stopping in {}s unless it frees space",
                    id,
                    name,
                    used / (1024 * 1024),
                    DISK_GRACE.saturating_sub(since.elapsed()).as_secs()
                );
            }
        }
    }

    /// Trim every running server's console log to its cap.
    pub async fn trim_console_logs(&self) {
        let max = std::env::var("NEXUS_CONSOLE_LOG_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > LOG_KEEP_BYTES)
            .unwrap_or(DEFAULT_LOG_MAX_BYTES);
        let ids: Vec<String> = self.states.read().await.keys().cloned().collect();
        for id in ids {
            if let Err(e) = self.runtime.trim_console_log(&id, max, LOG_KEEP_BYTES).await {
                warn!("Could not trim console log for {}: {}", id, e);
            }
        }
    }

    /// Run disk accounting and log trimming for the life of the process.
    pub fn start_maintenance(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(MAINTENANCE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                this.refresh_disk_usage().await;
                this.trim_console_logs().await;
            }
        });
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
        self.remove_blueprint(container_id).await;

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
        chown_path(&server_dir, self.game_user);

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

    /// Replace a server's blueprint and rebuild its container to match.
    ///
    /// This is how a billing package change reaches the runtime: resource
    /// limits and environment are baked into the container spec at create
    /// time, so applying new ones means recreating the container. The server's
    /// files are untouched — they live in the data directory, not the
    /// container — and so is its install state. A running server is stopped
    /// first; the return value says whether it was, so the caller can start it
    /// again.
    pub async fn reconfigure_container(
        &self,
        container_id: &str,
        config: &GameConfig,
    ) -> Result<bool> {
        let start = Instant::now();
        info!("Reconfiguring container: {}", container_id);

        let mut state = {
            let states = self.states.read().await;
            states
                .get(container_id)
                .ok_or_else(|| {
                    self.metrics.record_container_operation(
                        "reconfigure",
                        "not_found",
                        start.elapsed(),
                    );
                    NodeError::ContainerNotFound(container_id.to_string())
                })?
                .clone()
        };

        let was_running = state.status.is_running();
        if was_running {
            self.stop_container(container_id, Some(30)).await?;
            state.status = ContainerStatus::Stopped;
            state.pid = None;
        }

        // A local presence check when the image is already here, a real pull
        // only when the package changed the image.
        self.runtime.pull_image(&config.container.image).await?;

        let server_dir = self.data_dir.join(container_id);
        let spec = Self::config_to_spec(config, &server_dir, self.game_user)?;

        // A missing runtime container is fine here: it is about to exist.
        let _ = self.runtime.delete(container_id).await;
        self.runtime.create(container_id, spec).await.map_err(|e| {
            self.metrics.record_container_operation("reconfigure", "error", start.elapsed());
            NodeError::StartFailed {
                container_id: container_id.to_string(),
                source: e.into(),
            }
        })?;

        state.name = config.metadata.name.clone();
        state.image = config.container.image.clone();
        state.disk_limit_bytes = parse_size(&config.resources.disk.min).unwrap_or(0);
        self.set_state(state).await;
        self.persist_blueprint(container_id, config).await;

        self.metrics
            .record_container_operation("reconfigure", "success", start.elapsed());
        info!("Container {} reconfigured", container_id);

        Ok(was_running)
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
    fn config_to_spec(
        config: &GameConfig,
        server_dir: &std::path::Path,
        user: (u32, u32),
    ) -> Result<ContainerSpec> {
        // The command line, with the server's variables rendered into it.
        let argv = render_argv(config)?;

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

        // Ports, resolved through the variables the same way the command
        // line is. Advisory with host networking, but at least true.
        let vars = crate::container::startup::startup_vars(config);
        let ports: Vec<PortMapping> = config
            .networking
            .ports
            .iter()
            .filter_map(|port| {
                let rendered = crate::install::substitute(&port.internal, &vars);
                let container_port = rendered.trim().parse::<u16>().ok()?;
                let protocol = match port.protocol {
                    nexus_config::Protocol::Tcp => "tcp",
                    nexus_config::Protocol::Udp => "udp",
                    nexus_config::Protocol::Both => "both",
                };
                Some(PortMapping {
                    container_port,
                    host_port: container_port,
                    protocol: protocol.to_string(),
                })
            })
            .collect();

        // ulimits: nofile has its own field; the rest ride along.
        let ulimits = config
            .performance
            .as_ref()
            .and_then(|p| p.kernel.as_ref())
            .map(|k| k.ulimits.clone())
            .unwrap_or_default();
        let nofile = ulimits
            .get("nofile")
            .copied()
            .filter(|n| *n > 0)
            .unwrap_or(crate::runtime::DEFAULT_NOFILE);
        let rlimits: Vec<Rlimit> = ulimits
            .iter()
            .filter(|(k, v)| k.as_str() != "nofile" && **v > 0)
            .filter_map(|(k, v)| {
                rlimit_name(k).map(|kind| Rlimit {
                    kind: kind.to_string(),
                    soft: *v,
                    hard: *v,
                })
            })
            .collect();

        // Parse resource limits
        let cpu_shares = config.resources.cpu.shares as u64;
        let cpu_millicores = Some(config.resources.cpu.max).filter(|m| *m > 0);
        let memory_bytes = parse_size(&config.resources.memory.max)?;
        let memory_swap_bytes = if let Some(ref swap) = config.resources.memory.swap {
            parse_size(swap)?
        } else {
            0
        };

        let security = SecurityOptions {
            uid: user.0,
            gid: user.1,
            capabilities: resolve_capabilities(
                &config.security.capabilities.drop,
                &config.security.capabilities.add,
            ),
            no_new_privileges: config.security.no_new_privileges,
            read_only_root: config.security.read_only_root,
            seccomp: SeccompProfile::parse(&config.security.seccomp_profile),
        };

        Ok(ContainerSpec {
            image: config.container.image.clone(),
            command: vec![],
            args: argv,
            env,
            working_dir: config.startup.working_dir.clone(),
            mounts,
            ports,
            resources: ResourceLimits {
                cpu_shares,
                cpu_millicores,
                memory_bytes,
                memory_swap_bytes,
                pids_limit: Some(config.security.pids_limit).filter(|p| *p > 0),
                rlimits,
                nofile,
            },
            security,
            hostname: server_hostname(config, server_dir),
        })
    }
}

/// A hostname for the container: the server's id, which is what appears in
/// logs, rather than the same word for every server on the node.
fn server_hostname(config: &GameConfig, server_dir: &Path) -> String {
    let id = server_dir.file_name().and_then(|f| f.to_str()).unwrap_or(&config.metadata.id);
    format!("nx-{}", short_id(id))
}

/// The first 12 characters of an id, for names that must stay short.
fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// `SIGINT`, `INT` or `2` → 2.
fn signal_number(name: &str) -> Option<i32> {
    let upper = name.trim().to_ascii_uppercase();
    if let Ok(n) = upper.parse::<i32>() {
        return (n > 0 && n < 65).then_some(n);
    }
    let short = upper.strip_prefix("SIG").unwrap_or(&upper);
    Some(match short {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "KILL" => libc::SIGKILL,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "TERM" => libc::SIGTERM,
        _ => return None,
    })
}

/// Give one path to the game's user. Best-effort: a node not running as root
/// (a development checkout) cannot, and its containers run as it anyway.
fn chown_path(path: &Path, user: (u32, u32)) {
    if let Err(e) = std::os::unix::fs::lchown(path, Some(user.0), Some(user.1)) {
        if e.kind() != std::io::ErrorKind::PermissionDenied {
            warn!("Could not chown {:?}: {}", path, e);
        }
    }
}

/// Give a whole tree to the game's user, symlinks included but not followed.
fn chown_recursive(root: &Path, user: (u32, u32)) {
    use std::os::unix::fs::MetadataExt;
    let mut changed = 0u64;
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().flatten() {
        let already =
            entry.metadata().map(|m| m.uid() == user.0 && m.gid() == user.1).unwrap_or(true);
        if already {
            continue;
        }
        match std::os::unix::fs::lchown(entry.path(), Some(user.0), Some(user.1)) {
            Ok(()) => changed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(e) => warn!("Could not chown {:?}: {}", entry.path(), e),
        }
    }
    if changed > 0 {
        info!(
            "Set ownership of {} entries under {:?} to {}:{}",
            changed, root, user.0, user.1
        );
    }
}

/// Read whatever a console has buffered right now into `sink`.
///
/// `read_line` returning `None` means "caught up", not "finished", so this
/// drains what is available and returns rather than blocking.
async fn drain_console<F>(
    console: &mut Option<Box<dyn crate::runtime::ConsoleStream>>,
    sink: &mut F,
) where
    F: FnMut(String) + Send,
{
    let Some(stream) = console.as_mut() else {
        return;
    };
    // Bounded so a chatty install cannot starve the exit check.
    for _ in 0..512 {
        match stream.read_line().await {
            Ok(Some(line)) => sink(line),
            Ok(None) => return,
            Err(_) => return,
        }
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
            // A container with no task. One that was never started is
            // Created; one we thought was running had its task reaped while
            // we were away, which is a stop with the exit code lost.
            if !state.status.is_suspended() {
                state.status = if state.status.is_running() {
                    ContainerStatus::Stopped
                } else {
                    ContainerStatus::Created
                };
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
    async fn test_blueprint_persists_and_is_removed_with_the_container() {
        let temp = TempDir::new().unwrap();
        let manager = ContainerManager::new(temp.path().to_path_buf());
        let config = create_test_config();
        let id = manager.create_container(&config, Some("bp-1".to_string())).await.unwrap();

        // The originating blueprint is recoverable after creation...
        let loaded = manager.load_blueprint(&id).await.expect("blueprint saved");
        assert_eq!(loaded.metadata.id, config.metadata.id);
        assert_eq!(loaded.startup.working_dir, config.startup.working_dir);
        assert!(manager.blueprint_path(&id).exists());

        // ...and goes away with the container.
        manager.delete_container(&id, true).await.unwrap();
        assert!(manager.load_blueprint(&id).await.is_none());
        assert!(!manager.blueprint_path(&id).exists());
    }

    #[tokio::test]
    async fn test_load_blueprint_is_none_for_unknown_container() {
        let temp = TempDir::new().unwrap();
        let manager = ContainerManager::new(temp.path().to_path_buf());
        // Containers created before blueprint persistence simply have none.
        assert!(manager.load_blueprint("never-created").await.is_none());
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

    // ── Runtime correctness ──────────────────────────────────────────

    /// A manager over a mock runtime we can also drive directly.
    fn mock_manager() -> (
        ContainerManager,
        Arc<crate::runtime::mock::MockRuntime>,
        TempDir,
    ) {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(crate::runtime::mock::MockRuntime::new());
        let manager = ContainerManager::with_runtime(runtime.clone(), dir.path().to_path_buf());
        (manager, runtime, dir)
    }

    fn config_with(startup_extra: &str, disk: &str) -> GameConfig {
        let yaml = format!(
            r#"
metadata: {{ id: t, name: Test Server, version: "1", game: test, author: t }}
container: {{ image: example/game:1 }}
resources:
  cpu: {{ min: 500, max: 1500, shares: 1024 }}
  memory: {{ min: 512Mi, max: 1Gi, swap: 256Mi }}
  disk: {{ min: {} }}
startup:
  command: ./game
  args: ['-port {{{{SERVER_PORT}}}}', '+name "{{{{NAME}}}}"']
  working_dir: /home/container
{}
variables:
  - {{ name: SERVER_PORT, description: p, default: "27015" }}
  - {{ name: NAME, description: n, default: "My Server" }}
networking:
  ports:
    - {{ name: game, internal: "{{{{SERVER_PORT}}}}", protocol: udp }}
security:
  capabilities: {{ drop: [ALL], add: [net_bind_service] }}
  pids_limit: 300
performance:
  kernel:
    ulimits: {{ nofile: 4096, nproc: 512, bogus: 1 }}
"#,
            disk, startup_extra
        );
        GameConfig::from_yaml(&yaml).unwrap()
    }

    async fn wait_for<F, Fut>(mut check: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..200 {
            if check().await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        false
    }

    #[test]
    fn spec_renders_the_command_line_and_confines_the_process() {
        let config = config_with("", "10Gi");
        let dir = Path::new("/data/abcdef123456789");
        let spec = ContainerManager::config_to_spec(&config, dir, (988, 988)).unwrap();

        assert_eq!(
            spec.args,
            vec!["./game", "-port", "27015", "+name", "My Server"]
        );
        assert_eq!(spec.hostname, "nx-abcdef123456");
        assert_eq!(spec.security.uid, 988);
        assert_eq!(spec.security.capabilities, vec!["CAP_NET_BIND_SERVICE"]);
        assert!(spec.security.no_new_privileges);
        assert_eq!(spec.security.seccomp, SeccompProfile::RuntimeDefault);
        assert_eq!(spec.resources.cpu_millicores, Some(1500));
        assert_eq!(spec.resources.memory_bytes, 1024 * 1024 * 1024);
        assert_eq!(spec.resources.memory_swap_bytes, 256 * 1024 * 1024);
        assert_eq!(spec.resources.pids_limit, Some(300));
        assert_eq!(spec.resources.nofile, 4096);
        assert_eq!(
            spec.resources.rlimits,
            vec![Rlimit {
                kind: "RLIMIT_NPROC".into(),
                soft: 512,
                hard: 512
            }],
            "unknown ulimit keys are dropped, nofile has its own field"
        );
        assert_eq!(spec.ports.len(), 1);
        assert_eq!(
            spec.ports[0].container_port, 27015,
            "templated ports resolve"
        );
    }

    #[test]
    fn signals_parse_by_name_or_number() {
        assert_eq!(signal_number("SIGINT"), Some(libc::SIGINT));
        assert_eq!(signal_number("int"), Some(libc::SIGINT));
        assert_eq!(signal_number("15"), Some(15));
        assert_eq!(signal_number("SIGBOGUS"), None);
        assert_eq!(signal_number("0"), None);
    }

    #[tokio::test]
    async fn a_crash_is_detected_and_the_server_restarted() {
        let (manager, runtime, _dir) = mock_manager();
        let config = config_with(
            "  restart: { on_crash: true, max_retries: 2, delay: 0s, reset_after: 10m }",
            "1Gi",
        );
        let id = manager.create_container(&config, None).await.unwrap();
        manager.start_container(&id).await.unwrap();

        // The game dies.
        runtime.simulate_exit(&id, 137).await.unwrap();

        // The watcher notices, records the crash, and brings it back.
        assert!(
            wait_for(|| async {
                let s = manager.get_state(&id).await.unwrap();
                s.crash_count == 1 && s.status.is_running()
            })
            .await,
            "expected a restart after the first crash"
        );

        runtime.simulate_exit(&id, 1).await.unwrap();
        assert!(
            wait_for(|| async {
                let s = manager.get_state(&id).await.unwrap();
                s.crash_count == 2 && s.status.is_running()
            })
            .await,
            "second crash within the allowance is also restarted"
        );

        // Third crash exceeds max_retries: it stays down, marked failed.
        runtime.simulate_exit(&id, 1).await.unwrap();
        assert!(
            wait_for(|| async {
                let s = manager.get_state(&id).await.unwrap();
                s.crash_count == 3 && s.status == ContainerStatus::Failed
            })
            .await
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let s = manager.get_state(&id).await.unwrap();
        assert_eq!(s.status, ContainerStatus::Failed, "no fourth start");
        assert_eq!(s.exit_code, Some(1));
    }

    #[tokio::test]
    async fn a_clean_exit_is_not_a_crash() {
        let (manager, runtime, _dir) = mock_manager();
        let id = manager.create_container(&config_with("", "1Gi"), None).await.unwrap();
        manager.start_container(&id).await.unwrap();

        runtime.simulate_exit(&id, 0).await.unwrap();
        assert!(
            wait_for(|| async { !manager.get_state(&id).await.unwrap().status.is_running() }).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let s = manager.get_state(&id).await.unwrap();
        assert_eq!(s.status, ContainerStatus::Stopped);
        assert_eq!(s.crash_count, 0);
    }

    #[tokio::test]
    async fn a_deliberate_stop_is_never_counted_as_a_crash() {
        let (manager, _runtime, _dir) = mock_manager();
        let id = manager.create_container(&config_with("", "1Gi"), None).await.unwrap();
        manager.start_container(&id).await.unwrap();
        manager.stop_container(&id, Some(1)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let s = manager.get_state(&id).await.unwrap();
        assert_eq!(s.status, ContainerStatus::Stopped);
        assert_eq!(s.crash_count, 0);
        // And a fresh start after that is watched anew.
        manager.start_container(&id).await.unwrap();
        assert!(manager.get_state(&id).await.unwrap().status.is_running());
    }

    #[tokio::test]
    async fn stop_sends_the_blueprints_pre_stop_commands_first() {
        let (manager, runtime, _dir) = mock_manager();
        let config = config_with(
            "  lifecycle:\n    pre_stop:\n      - { type: send_command, command: save-all, delay: 10ms }\n      - { type: send_command, command: stop }",
            "1Gi",
        );
        let id = manager.create_container(&config, None).await.unwrap();
        manager.start_container(&id).await.unwrap();
        manager.stop_container(&id, Some(5)).await.unwrap();

        assert_eq!(runtime.commands_sent(&id).await, vec!["save-all", "stop"]);
        assert!(
            runtime.signals_sent(&id).await.is_empty(),
            "no signal was needed"
        );
        assert_eq!(
            manager.get_state(&id).await.unwrap().status,
            ContainerStatus::Stopped
        );
    }

    #[tokio::test]
    async fn stop_uses_the_blueprints_signal_when_there_are_no_commands() {
        let (manager, runtime, _dir) = mock_manager();
        let id = manager
            .create_container(&config_with("  stop_signal: SIGINT", "1Gi"), None)
            .await
            .unwrap();
        manager.start_container(&id).await.unwrap();
        manager.stop_container(&id, Some(5)).await.unwrap();
        assert_eq!(runtime.signals_sent(&id).await, vec![libc::SIGINT]);
        assert!(runtime.commands_sent(&id).await.is_empty());
    }

    #[tokio::test]
    async fn a_suspended_server_cannot_be_started() {
        let (manager, _runtime, _dir) = mock_manager();
        let id = manager.create_container(&config_with("", "1Gi"), None).await.unwrap();
        manager.suspend_container(&id).await.unwrap();
        let err = manager.start_container(&id).await.unwrap_err();
        assert!(err.to_string().contains("suspended"));
    }

    #[tokio::test]
    async fn disk_usage_is_measured_and_gates_starting() {
        let (manager, _runtime, dir) = mock_manager();
        // A 1 MiB allowance.
        let id = manager.create_container(&config_with("", "1Mi"), None).await.unwrap();
        assert_eq!(
            manager.get_state(&id).await.unwrap().disk_limit_bytes,
            1024 * 1024
        );

        std::fs::write(
            dir.path().join(&id).join("world.dat"),
            vec![0u8; 2 * 1024 * 1024],
        )
        .unwrap();
        manager.refresh_disk_usage().await;
        let s = manager.get_state(&id).await.unwrap();
        assert_eq!(s.disk_used_bytes, 2 * 1024 * 1024);
        assert!(s.disk_exceeded());

        let err = manager.start_container(&id).await.unwrap_err();
        assert!(err.to_string().contains("disk allowance"), "{}", err);

        // Under the limit again: starts.
        std::fs::remove_file(dir.path().join(&id).join("world.dat")).unwrap();
        manager.refresh_disk_usage().await;
        manager.start_container(&id).await.unwrap();

        // Going over while running is tolerated for the grace period.
        std::fs::write(
            dir.path().join(&id).join("world.dat"),
            vec![0u8; 2 * 1024 * 1024],
        )
        .unwrap();
        manager.refresh_disk_usage().await;
        assert!(manager.get_state(&id).await.unwrap().status.is_running());
    }

    #[tokio::test]
    async fn restore_rearms_the_watcher_for_a_still_running_server() {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(crate::runtime::mock::MockRuntime::new());
        let first = ContainerManager::with_runtime(runtime.clone(), dir.path().to_path_buf());
        let id = first.create_container(&config_with("", "1Gi"), None).await.unwrap();
        first.start_container(&id).await.unwrap();

        // A "new node" over the same runtime and data dir.
        let second = ContainerManager::with_runtime(runtime.clone(), dir.path().to_path_buf());
        assert_eq!(second.restore().await, 1);
        assert!(second.get_state(&id).await.unwrap().status.is_running());

        runtime.simulate_exit(&id, 3).await.unwrap();
        assert!(
            wait_for(|| async { second.get_state(&id).await.unwrap().crash_count == 1 }).await,
            "the restored manager's watcher saw the crash"
        );
    }

    #[tokio::test]
    async fn restore_rebuilds_a_container_the_runtime_lost() {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(crate::runtime::mock::MockRuntime::new());
        let first = ContainerManager::with_runtime(runtime.clone(), dir.path().to_path_buf());
        let id = first.create_container(&config_with("", "1Gi"), None).await.unwrap();

        // containerd forgets it (a namespace wipe).
        runtime.delete(&id).await.unwrap();
        assert!(!runtime.exists(&id).await);

        let second = ContainerManager::with_runtime(runtime.clone(), dir.path().to_path_buf());
        second.restore().await;
        assert!(
            runtime.exists(&id).await,
            "rebuilt from the stored blueprint"
        );
        second.start_container(&id).await.unwrap();
    }
}

use crate::install::InstallState;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// Container status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerStatus {
    /// Container is created but not started
    Created,
    /// Container is running
    Running,
    /// Container is paused
    Paused,
    /// Container has stopped
    Stopped,
    /// Container has exited with an error
    Failed,
    /// Container is suspended (stopped but marked for billing purposes)
    Suspended,
}

impl ContainerStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, ContainerStatus::Running)
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, ContainerStatus::Stopped | ContainerStatus::Failed)
    }

    pub fn is_suspended(&self) -> bool {
        matches!(self, ContainerStatus::Suspended)
    }
}

/// Container state information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    /// Container ID
    pub id: String,

    /// Server name
    pub name: String,

    /// Container image
    pub image: String,

    /// Current status
    pub status: ContainerStatus,

    /// Container PID (if running)
    pub pid: Option<u32>,

    /// Exit code (if stopped)
    pub exit_code: Option<i32>,

    /// Restart count
    pub restart_count: u32,

    /// Created at timestamp
    pub created_at: SystemTime,

    /// Started at timestamp (if ever started)
    pub started_at: Option<SystemTime>,

    /// Stopped at timestamp (if stopped)
    pub stopped_at: Option<SystemTime>,

    /// Where this server stands with respect to its game files.
    ///
    /// Defaults to `Pending` for states written before installs existed: a
    /// server restored from such a file has unknown provenance, and treating
    /// it as installed would be the dangerous direction of that guess only if
    /// it were not — so callers reconcile it against the blueprint on restore.
    #[serde(default)]
    pub install_state: InstallState,
}

impl ContainerState {
    pub fn new(id: String, name: String, image: String) -> Self {
        Self {
            id,
            name,
            image,
            status: ContainerStatus::Created,
            pid: None,
            exit_code: None,
            restart_count: 0,
            created_at: SystemTime::now(),
            started_at: None,
            stopped_at: None,
            install_state: InstallState::Pending,
        }
    }

    pub fn mark_started(&mut self, pid: u32) {
        self.status = ContainerStatus::Running;
        self.pid = Some(pid);
        self.started_at = Some(SystemTime::now());
        self.stopped_at = None;
    }

    pub fn mark_stopped(&mut self, exit_code: i32) {
        self.status = if exit_code == 0 {
            ContainerStatus::Stopped
        } else {
            ContainerStatus::Failed
        };
        self.exit_code = Some(exit_code);
        self.stopped_at = Some(SystemTime::now());
        self.pid = None;
    }

    pub fn mark_restarted(&mut self) {
        self.restart_count += 1;
    }

    pub fn mark_suspended(&mut self) {
        self.status = ContainerStatus::Suspended;
        self.pid = None;
        self.stopped_at = Some(SystemTime::now());
    }

    pub fn mark_unsuspended(&mut self) {
        self.status = ContainerStatus::Stopped;
        // Container is now in stopped state, ready to be started
    }
}

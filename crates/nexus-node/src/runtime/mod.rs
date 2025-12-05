pub mod mock;

use crate::error::Result;
use async_trait::async_trait;
use std::collections::HashMap;

/// Container runtime abstraction
/// Allows for different implementations (Containerd, Docker, Mock)
#[async_trait]
pub trait ContainerRuntime: Send + Sync {
    /// Pull a container image
    async fn pull_image(&self, image: &str) -> Result<()>;

    /// Create a container from spec
    async fn create(
        &self,
        id: &str,
        spec: ContainerSpec,
    ) -> Result<ContainerInfo>;

    /// Start a container
    async fn start(&self, id: &str) -> Result<u32>; // Returns PID

    /// Stop a container
    async fn stop(&self, id: &str, timeout_secs: u32) -> Result<i32>; // Returns exit code

    /// Delete a container
    async fn delete(&self, id: &str) -> Result<()>;

    /// Get container info
    async fn inspect(&self, id: &str) -> Result<ContainerInfo>;

    /// Attach to container console (stdout/stderr)
    async fn attach(&self, id: &str) -> Result<Box<dyn ConsoleStream>>;
}

/// Container specification
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub image: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_dir: String,
    pub mounts: Vec<Mount>,
    pub ports: Vec<PortMapping>,
    pub resources: ResourceLimits,
}

/// Mount point
#[derive(Debug, Clone)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

/// Port mapping
#[derive(Debug, Clone)]
pub struct PortMapping {
    pub container_port: u16,
    pub host_port: u16,
    pub protocol: String,
}

/// Resource limits
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub cpu_shares: u64,
    pub memory_bytes: u64,
    pub memory_swap_bytes: u64,
}

/// Container information
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub pid: Option<u32>,
    pub status: String,
    pub exit_code: Option<i32>,
}

/// Console stream (stdout/stderr)
#[async_trait]
pub trait ConsoleStream: Send {
    async fn read_line(&mut self) -> Result<Option<String>>;
}

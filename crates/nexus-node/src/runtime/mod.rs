pub mod containerd;
pub mod image;
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
    async fn create(&self, id: &str, spec: ContainerSpec) -> Result<ContainerInfo>;

    /// Start a container
    async fn start(&self, id: &str) -> Result<u32>; // Returns PID

    /// Stop a container
    async fn stop(&self, id: &str, timeout_secs: u32) -> Result<i32>; // Returns exit code

    /// Block until a container's process exits, returning its exit code.
    ///
    /// This is what a one-shot container needs — an install script that runs
    /// to completion — as opposed to `stop`, which asks a long-running server
    /// to shut down. Errors if the container is still running when `timeout`
    /// elapses.
    async fn wait(&self, id: &str, timeout: std::time::Duration) -> Result<i32>;

    /// Delete a container
    async fn delete(&self, id: &str) -> Result<()>;

    /// Get container info
    async fn inspect(&self, id: &str) -> Result<ContainerInfo>;

    /// Attach to container console (stdout/stderr) - read only
    async fn attach(&self, id: &str) -> Result<Box<dyn ConsoleStream>>;

    /// Attach to container console (bidirectional stdin/stdout/stderr)
    async fn attach_bidirectional(&self, id: &str) -> Result<Box<dyn BidirectionalConsole>>;

    /// Send a single command to container stdin
    async fn send_command(&self, id: &str, command: &str) -> Result<()>;

    /// Execute a one-shot command as a new process inside a running container
    /// and return its captured output. This is distinct from `send_command`
    /// (which writes to the game process's stdin / RCON): `exec` spawns a
    /// separate process, so it can run arbitrary tooling (e.g. `npm`, a shell).
    ///
    /// The whole call is bounded by `timeout`; callers pass a short bound for
    /// interactive use (the shell console) and a long one for background jobs
    /// (a game-file update that can run for many minutes).
    async fn exec(
        &self,
        id: &str,
        command: &[String],
        timeout: std::time::Duration,
    ) -> Result<ExecOutput>;
}

/// Default timeout for interactive `exec` calls (the shell console).
pub const DEFAULT_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Result of a one-shot `exec` inside a container.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Process exit code (`None` if it could not be determined).
    pub exit_code: Option<i32>,
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
    /// Maximum open file descriptors (`RLIMIT_NOFILE`).
    ///
    /// Game servers hold a descriptor per connected player, per world region
    /// file and per log; SteamCMD raises this to 2048 for itself and warns
    /// when it cannot. The old fixed 1024 was below what the tooling asks for
    /// before a single player connects.
    pub nofile: u64,
}

/// Open-file limit for containers whose blueprint does not set one.
///
/// Matches what container runtimes hand a service by default, and is far
/// enough above any game's needs not to be the thing that breaks.
pub const DEFAULT_NOFILE: u64 = 65536;

/// Container information
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub pid: Option<u32>,
    pub status: String,
    pub exit_code: Option<i32>,
}

/// Console stream (stdout/stderr) - read only
#[async_trait]
pub trait ConsoleStream: Send {
    async fn read_line(&mut self) -> Result<Option<String>>;
}

/// Bidirectional console stream (stdin/stdout/stderr)
#[async_trait]
pub trait BidirectionalConsole: Send {
    /// Read data from console (stdout/stderr combined)
    async fn read(&mut self) -> Result<Option<Vec<u8>>>;

    /// Write data to console (stdin)
    async fn write(&mut self, data: &[u8]) -> Result<()>;

    /// Resize the terminal
    async fn resize(&mut self, rows: u16, cols: u16) -> Result<()>;

    /// Close the console connection
    async fn close(&mut self) -> Result<()>;

    /// Check if console is still open
    fn is_open(&self) -> bool;
}

/// Console command sender (for one-shot commands without full attach)
#[async_trait]
pub trait CommandSender: Send + Sync {
    /// Send a command to container stdin and return immediately
    async fn send_command(&self, container_id: &str, command: &str) -> Result<()>;
}

/// Log entry from container
#[derive(Debug, Clone)]
pub struct LogLine {
    pub timestamp: String,
    pub stream: LogStream,
    pub line: String,
}

/// Log stream type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

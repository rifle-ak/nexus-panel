pub mod containerd;
pub mod image;
pub mod mock;
pub mod seccomp;

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

    /// Send a signal to a container's main process without waiting for it.
    ///
    /// This is how a blueprint asks for a shutdown by `SIGINT` rather than
    /// `SIGTERM`; the caller follows it with [`wait`](Self::wait) and, if the
    /// process ignores it, [`stop`](Self::stop).
    async fn kill(&self, id: &str, signal: i32) -> Result<()>;

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

    /// Keep a container's console log from growing without bound.
    ///
    /// When the log is larger than `max_bytes`, only its last `keep_bytes`
    /// are kept. A runtime that does not keep a log of its own has nothing to
    /// do.
    async fn trim_console_log(&self, _id: &str, _max_bytes: u64, _keep_bytes: u64) -> Result<()> {
        Ok(())
    }

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

    /// What a running container is using right now: cumulative CPU time,
    /// memory, process count and block I/O, as its cgroup reports them.
    ///
    /// Counters are cumulative; a sampler turns two readings into rates.
    /// A container that is not running has no cgroup and errors.
    async fn stats(&self, id: &str) -> Result<ContainerStats>;

    /// The last `max_bytes` of a container's console output, for a console
    /// that opens on a server already running. A runtime that keeps no log
    /// returns an empty string.
    async fn console_tail(&self, _id: &str, _max_bytes: u64) -> Result<String> {
        Ok(String::new())
    }

    /// Round-trip to the runtime daemon, for health checks. Errors when the
    /// daemon is unreachable.
    async fn ping(&self) -> Result<()> {
        Ok(())
    }
}

/// A point-in-time reading of a container's cgroup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerStats {
    /// CPU time consumed since the container started, in microseconds
    /// (user + system).
    pub cpu_usage_usec: u64,
    /// Memory in use, in bytes (`memory.current`: anonymous + page cache).
    pub memory_bytes: u64,
    /// Anonymous memory alone, in bytes: what the game really holds, as
    /// opposed to file cache the kernel can drop.
    pub memory_anon_bytes: u64,
    /// The memory limit in bytes; `None` when unlimited.
    pub memory_limit_bytes: Option<u64>,
    /// Processes and threads in the container.
    pub pids: u64,
    /// Bytes read from block devices since start.
    pub io_read_bytes: u64,
    /// Bytes written to block devices since start.
    pub io_write_bytes: u64,
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
    /// Who the process is and what it may do.
    pub security: SecurityOptions,
    /// The container's hostname. Empty means the runtime picks one.
    pub hostname: String,
}

/// How a container's process is confined.
#[derive(Debug, Clone)]
pub struct SecurityOptions {
    /// User the process runs as. Game servers run as an unprivileged user;
    /// only the one-shot install container, which has to write files the
    /// game will later own, runs as root.
    pub uid: u32,
    pub gid: u32,
    /// Capabilities in the bounding set, as `CAP_*` names. For a non-root
    /// user they are also made ambient, so a server may still bind a port
    /// below 1024 when its blueprint grants `NET_BIND_SERVICE`.
    pub capabilities: Vec<String>,
    /// Forbid `setuid`/file-capability escalation on exec.
    pub no_new_privileges: bool,
    /// Mount the image's root filesystem read-only. The server's own
    /// directory is a separate writable mount either way.
    pub read_only_root: bool,
    pub seccomp: SeccompProfile,
}

/// Which seccomp filter a container runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeccompProfile {
    /// The standard container allowlist (see [`seccomp::default_profile`]).
    RuntimeDefault,
    /// No filter. Only for a workload that provably needs a blocked syscall.
    Unconfined,
    /// An OCI seccomp profile read from this path on the node.
    Path(String),
}

impl SeccompProfile {
    /// Parse a blueprint's `security.seccomp_profile`.
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "" | "runtime/default" | "default" => SeccompProfile::RuntimeDefault,
            "unconfined" | "none" => SeccompProfile::Unconfined,
            path => SeccompProfile::Path(path.to_string()),
        }
    }
}

/// The capabilities a container gets when its blueprint does not say
/// otherwise: what Docker and containerd grant by default, which is enough
/// for an ordinary service and includes nothing that reaches the host.
pub const DEFAULT_CAPABILITIES: &[&str] = &[
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_NET_BIND_SERVICE",
    "CAP_SYS_CHROOT",
    "CAP_AUDIT_WRITE",
    "CAP_MKNOD",
];

/// User game servers run as when `NEXUS_CONTAINER_UID`/`GID` are unset.
///
/// Matches the id Pterodactyl's Wings uses, so a node migrated from it keeps
/// its file ownership.
pub const DEFAULT_CONTAINER_UID: u32 = 988;
pub const DEFAULT_CONTAINER_GID: u32 = 988;

/// The unprivileged user game servers run as, from the environment.
pub fn container_user() -> (u32, u32) {
    let read = |name: &str, default: u32| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(default)
    };
    (
        read("NEXUS_CONTAINER_UID", DEFAULT_CONTAINER_UID),
        read("NEXUS_CONTAINER_GID", DEFAULT_CONTAINER_GID),
    )
}

/// Turn a blueprint capability name (`NET_BIND_SERVICE`, `cap_sys_nice`) into
/// the `CAP_*` form the OCI spec wants.
pub fn normalize_capability(name: &str) -> String {
    let upper = name.trim().to_ascii_uppercase();
    if upper.starts_with("CAP_") {
        upper
    } else {
        format!("CAP_{}", upper)
    }
}

/// Apply a blueprint's `capabilities.drop` then `capabilities.add` to the
/// default set. `ALL` in `drop` empties it first.
pub fn resolve_capabilities(drop: &[String], add: &[String]) -> Vec<String> {
    let mut caps: Vec<String> = DEFAULT_CAPABILITIES.iter().map(|c| c.to_string()).collect();
    for name in drop {
        if name.trim().eq_ignore_ascii_case("all") {
            caps.clear();
        } else {
            let cap = normalize_capability(name);
            caps.retain(|c| c != &cap);
        }
    }
    for name in add {
        if name.trim().eq_ignore_ascii_case("all") {
            // "Add everything" is never what a game server needs; keep the
            // default set rather than hand the host over.
            continue;
        }
        let cap = normalize_capability(name);
        if !caps.contains(&cap) {
            caps.push(cap);
        }
    }
    caps
}

impl Default for SecurityOptions {
    fn default() -> Self {
        let (uid, gid) = container_user();
        Self {
            uid,
            gid,
            capabilities: DEFAULT_CAPABILITIES.iter().map(|c| c.to_string()).collect(),
            no_new_privileges: true,
            read_only_root: false,
            seccomp: SeccompProfile::RuntimeDefault,
        }
    }
}

impl SecurityOptions {
    /// The options the one-shot install container runs with: root, so the
    /// blueprint's script can lay files down wherever the game expects them,
    /// but still no new privileges and the default seccomp filter.
    pub fn for_install() -> Self {
        Self {
            uid: 0,
            gid: 0,
            ..Self::default()
        }
    }
}

/// A resource limit (`ulimit`) applied to the container's process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rlimit {
    /// `RLIMIT_NOFILE`, `RLIMIT_NPROC`, …
    pub kind: String,
    pub soft: u64,
    pub hard: u64,
}

/// Map a blueprint `ulimits` key to its `RLIMIT_*` name.
pub fn rlimit_name(key: &str) -> Option<&'static str> {
    Some(match key.trim().to_ascii_lowercase().as_str() {
        "nofile" => "RLIMIT_NOFILE",
        "nproc" => "RLIMIT_NPROC",
        "memlock" => "RLIMIT_MEMLOCK",
        "core" => "RLIMIT_CORE",
        "stack" => "RLIMIT_STACK",
        "fsize" => "RLIMIT_FSIZE",
        "msgqueue" => "RLIMIT_MSGQUEUE",
        "rtprio" => "RLIMIT_RTPRIO",
        "nice" => "RLIMIT_NICE",
        "sigpending" => "RLIMIT_SIGPENDING",
        "locks" => "RLIMIT_LOCKS",
        "as" | "vmem" => "RLIMIT_AS",
        "data" => "RLIMIT_DATA",
        "cpu" => "RLIMIT_CPU",
        "rss" => "RLIMIT_RSS",
        "rttime" => "RLIMIT_RTTIME",
        _ => return None,
    })
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
    /// Relative CPU weight against other containers under contention.
    pub cpu_shares: u64,
    /// Hard CPU cap in millicores (1000 = one core), or `None` for no cap.
    pub cpu_millicores: Option<u32>,
    pub memory_bytes: u64,
    /// Swap the container may use on top of its memory limit. Zero means no
    /// swap: a server sold N GiB is not quietly given less than N GiB of RAM.
    pub memory_swap_bytes: u64,
    /// Most processes and threads at once, or `None` for the kernel default.
    pub pids_limit: Option<u32>,
    /// Extra `ulimit`s beyond `nofile`.
    pub rlimits: Vec<Rlimit>,
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

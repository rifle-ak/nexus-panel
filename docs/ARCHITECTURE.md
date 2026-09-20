# Technical Architecture

Detailed architecture of the Nexus Panel system.

## System Overview

```
┌─────────────────────────────────────────────────────────────┐
│                        Panel API                            │
└────────────────────────┬────────────────────────────────────┘
                         │ gRPC/mTLS
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                      Nexus Node Daemon                      │
│                                                             │
│  ┌──────────────┐  ┌─────────────┐  ┌──────────────┐       │
│  │   gRPC API   │  │  Container  │  │   Resource   │       │
│  │   Server     │──│   Manager   │──│   Monitor    │       │
│  └──────────────┘  └─────────────┘  └──────────────┘       │
│         │                 │                 │               │
│  ┌──────▼──────┐  ┌───────▼─────┐  ┌────────▼─────┐       │
│  │   Console   │  │  Lifecycle  │  │  Prometheus  │       │
│  │  Streaming  │  │   Manager   │  │   Metrics    │       │
│  └─────────────┘  └─────────────┘  └──────────────┘       │
│                          │                                  │
└──────────────────────────┼──────────────────────────────────┘
                           │ Unix Socket
                           ▼
                 ┌──────────────────┐
                 │    Containerd    │
                 └────────┬─────────┘
                          │
                 ┌────────▼─────────┐
                 │   runc (OCI)     │
                 └──────────────────┘
```

## Core Components

### Container Manager

Manages container lifecycle and state.

**Responsibilities:**
- Load and parse YAML configs
- Create containers with OCI specs
- Manage lifecycle (create, start, stop, restart, delete)
- Apply resource limits
- Track container state

**Key Features:**
- Idempotent operations
- Graceful degradation
- Automatic restart policies
- Health checking

### Containerd Runtime

Production container runtime integration.

**Implementation:** `crates/nexus-node/src/runtime/containerd.rs`

```rust
pub trait ContainerRuntime: Send + Sync {
    async fn pull_image(&self, image: &str) -> Result<()>;
    async fn create(&self, id: &str, spec: ContainerSpec) -> Result<ContainerInfo>;
    async fn start(&self, id: &str) -> Result<u32>;
    async fn stop(&self, id: &str, timeout_secs: u32) -> Result<i32>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn inspect(&self, id: &str) -> Result<ContainerInfo>;
    async fn attach(&self, id: &str) -> Result<Box<dyn ConsoleStream>>;
}
```

**OCI Spec Generation:**
- Process configuration (command, args, environment)
- Resource limits (CPU shares, memory limits)
- Linux namespaces (PID, IPC, UTS, mount, network)
- Minimal capabilities
- Bind mounts for server data

### gRPC Server

Type-safe API for remote management.

**Implementation:** `crates/nexus-node/src/grpc/server.rs`

**Features:**
- Full async/await support
- mTLS authentication
- Rate limiting
- Request tracing
- Audit logging

### File Manager

Secure file operations within containers.

**Implementation:** `crates/nexus-node/src/files.rs`

**Operations:**
- List, read, write, delete files
- Create directories
- Rename, copy files
- Compress/decompress archives
- SHA256 checksums
- Directory traversal protection

### Backup Manager

Container data backup and restore.

**Implementation:** `crates/nexus-node/src/backup.rs`

- A backup is a gzip tar of the server directory, or of the blueprint's
  `backups.paths` minus `backups.exclude`, with a SHA-256 checksum, under
  `DATA_DIR/backups/<server>/<id>.tar.gz`.
- Each archive has an `<id>.json` record beside it; the registry is
  rebuilt from those at start, an archive without a record is adopted
  (restorable, no checksum to verify), and a record without an archive is
  dropped.
- `backup_server` is the one path everything takes: the blueprint's
  `pre_backup_command` (`save-all`) goes to a running server first, then
  the archive, then `backups.retention` deletes the oldest beyond N.
- Restore verifies the checksum, then unpacks beside the server directory
  and swaps the two, so a failure leaves the server as it was. Files come
  out owned by the game user. A running server is refused unless the
  caller asks to stop it.

### Schedule Manager

Cron-based task automation.

**Implementation:** `crates/nexus-node/src/schedule.rs`

- Five-field cron (`0 4 * * *`); six and seven fields also accepted. Each
  schedule may name an IANA time zone; otherwise UTC.
- The runner ticks every second. A due schedule has its next run computed
  at once and executes on its own task, so a task's `time_offset` and a
  slow backup delay that schedule alone. A schedule still running when it
  is due again is skipped for that run, and "Run now" refuses to stack.
- Tasks: console command, power action (start/stop/restart), backup
  (through `backup_server`). The first failure is kept as `last_error`.
- Persisted as JSON under `DATA_DIR/.nexus/schedules`.

## Security Model

### Container Isolation

**Linux Namespaces:**
- PID namespace (isolated process tree)
- Network namespace (isolated networking)
- Mount namespace (isolated filesystem)
- User namespace (UID/GID mapping)
- IPC namespace (isolated IPC)

**Cgroups v2:**
- CPU quota
- Memory limits
- Disk I/O throttling
- Network bandwidth limits

### Capability Management

Default policy: Drop ALL, add only required.

**Granted Capabilities:**
- `CAP_CHOWN` - File ownership
- `CAP_DAC_OVERRIDE` - File access
- `CAP_FOWNER` - File operations
- `CAP_SETUID/SETGID` - User switching
- `CAP_NET_BIND_SERVICE` - Bind ports <1024

### Firewall (nftables)

Game servers share the host's network namespace, so their traffic is
filtered in the host's packet path. `firewall.rs` owns one nftables table,
`inet nexus`, and keeps it in step with what is running. Every change is a
single atomic `nft -f` transaction.

**Node-wide, always on:**
- Trusted list (never filtered) and blocklist (timed or permanent), as
  interval sets, so a whole network is one element
- `ct state invalid` drops; established TCP is accepted before any meter
- Per-source SYN meter and a global SYN ceiling on every game port
- Per-source UDP packet meter on every game port
- Kernel tuning: SYN cookies, deep SYN backlog, larger conntrack table

**Per server, while it runs:** a chain `srv_<id>` holding the blueprint's
`security.firewall_rules` (connection rate, packet size, allowed and
blocked CIDRs). Verdict maps keyed by port dispatch to it, so each server's
rules cost one lookup and its counters are its own. Rules carry a comment
the status reader keys on, which is how the panel shows what each dropped.

Without `nft` or root the firewall is disabled and says so at startup;
everything else runs unchanged.

## Data Flow

### Container Start

```
1. Client → gRPC: StartContainer(id)
2. gRPC → ContainerManager: start(id)
3. ContainerManager → Runtime: start(id)
4. Runtime → Containerd: CreateTask
5. Containerd → runc: start
6. ContainerManager → Metrics: track
7. gRPC → Client: success
```

### Log Streaming

```
1. Client → gRPC: StreamLogs(id)
2. gRPC → ContainerManager: attach(id)
3. ContainerManager → Runtime: attach(id)
4. Runtime → Container stdout/stderr
5. Stream → gRPC → Client
```

The panel's console does the same over HTTP: `GET …/console` returns the
tail of the log the shim writes, and `GET …/console/stream` follows it as
server-sent events, one event per line, polling the file every 250 ms
while there is nothing new.

### Resource Monitoring

`stats.rs` samples every running server and the node every 5 seconds:

```
1. ResourceMonitor → ContainerManager: list_containers()
2. For each running server → Runtime: stats(id)
3. Runtime → /proc/<pid>/cgroup → /sys/fs/cgroup/<path>/{cpu.stat,
   memory.current, memory.max, memory.stat, pids.current, io.stat}
4. Monitor → rates from the previous reading; 120-sample history;
   Prometheus gauges
5. Web: /containers/:id/stats, /node/stats, `usage` on container JSON
```

cgroup v1 hosts are read through their per-controller hierarchies. Network
is not per server: servers share the host network namespace.

## Crate Structure

```
crates/
├── nexus-node/           # Main daemon
│   ├── src/
│   │   ├── bin/          # Binary entry points
│   │   ├── grpc/         # gRPC server
│   │   ├── runtime/      # Container runtime
│   │   ├── container/    # Container management
│   │   ├── files.rs      # File operations
│   │   ├── backup.rs     # Backup system
│   │   ├── schedule.rs   # Task scheduling
│   │   ├── auth.rs       # Authentication
│   │   ├── tls.rs        # TLS/mTLS
│   │   └── metrics.rs    # Prometheus metrics
│   └── proto/            # Protobuf definitions
│
├── nexus-config/         # Config format
│   └── src/
│       ├── lib.rs        # GameConfig struct
│       └── diagnostics.rs
│
├── nexus-marketplace/    # Mod marketplace
│   └── src/
│       ├── adapters/     # Umod, Codefling, Lone.Design, Steam Workshop
│       ├── models.rs     # Data structures
│       └── cache.rs      # Metadata caching
│
└── egg-importer/         # Pterodactyl converter
    └── src/
        ├── pterodactyl.rs
        └── converter.rs
```

The WHMCS integration is not a crate: WHMCS calls a PHP provisioning module,
which calls the node. The module lives in `whmcs/modules/servers/nexuspanel`
and the node side is `nexus-node/src/provision.rs` (records, port allocation,
blueprint overrides) plus the `/api/v1/provision` routes in
`nexus-node/src/web/provision.rs`. Customer sign-in from WHMCS produces a
session scoped to one server (`web/auth.rs`, `SessionScope`), which the auth
middleware enforces on every route.

## File System Layout

```
/var/lib/nexus-node/
├── containers/
│   ├── {container-id}/
│   │   ├── config.yaml
│   │   ├── data/           # Server files
│   │   └── logs/           # Archived logs
│   └── ...
├── backups/
│   ├── {container-id}/
│   │   ├── {backup-id}.tar.gz
│   │   └── ...
│   └── ...
└── cache/
    └── images/
```

## Performance Targets

| Operation | Target |
|-----------|--------|
| Container cold start | <2 seconds |
| Container warm start | <500ms |
| Container stop | <5 seconds |
| gRPC request latency | <10ms (p99) |
| Memory per container | <20MB overhead |
| Max containers/node | 1000+ |

## Technology Stack

- **Runtime**: Tokio async
- **gRPC**: Tonic + Prost
- **Container**: Containerd client
- **Metrics**: Prometheus
- **Serialization**: Serde + YAML
- **Logging**: Tracing
- **Error Handling**: anyhow + thiserror

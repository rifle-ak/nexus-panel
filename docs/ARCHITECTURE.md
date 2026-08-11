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

**Features:**
- Compressed tar.gz backups
- SHA256 verification
- Include/exclude patterns
- Background backup creation
- Integrity-verified restore

### Schedule Manager

Cron-based task automation.

**Implementation:** `crates/nexus-node/src/schedule.rs`

**Task Types:**
- Console commands
- Power actions (start, stop, restart)
- Backup creation
- Task chaining with offsets

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

### XDP Firewall

eBPF-based packet filtering for DDoS protection.

**Rules:**
- Connection rate limiting
- Packet size limits
- Per-port filtering
- IP blacklisting

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
├── nexus-whmcs/          # WHMCS integration
│   └── src/
│       ├── provisioning.rs
│       ├── billing.rs
│       ├── hooks.rs
│       └── sso.rs
│
└── egg-importer/         # Pterodactyl converter
    └── src/
        ├── pterodactyl.rs
        └── converter.rs
```

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

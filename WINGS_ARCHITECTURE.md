# Wings Daemon Architecture

**Nexus Panel Wings** - High-performance game server runtime

## Overview

Wings is the daemon component of Nexus Panel that manages game server containers. It reads the converted YAML configs and runs them in isolated containers with resource limits, networking, and monitoring.

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                        Panel API                             │
│                    (Future Component)                        │
└────────────────────────┬────────────────────────────────────┘
                         │ gRPC
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                      Wings Daemon                            │
│                                                              │
│  ┌──────────────┐  ┌─────────────┐  ┌──────────────┐      │
│  │   gRPC API   │  │  Container  │  │   Resource   │      │
│  │   Server     │──│   Manager   │──│   Monitor    │      │
│  └──────────────┘  └─────────────┘  └──────────────┘      │
│         │                  │                  │             │
│         │                  │                  │             │
│  ┌──────▼──────┐  ┌───────▼─────┐  ┌────────▼─────┐      │
│  │  Console    │  │  Lifecycle  │  │  Prometheus  │      │
│  │  Streaming  │  │   Manager   │  │   Metrics    │      │
│  └─────────────┘  └─────────────┘  └──────────────┘      │
│                           │                                 │
└───────────────────────────┼─────────────────────────────────┘
                            │ CRI API
                            ▼
                  ┌──────────────────┐
                  │   Containerd     │
                  └──────────────────┘
                            │
                            ▼
                  ┌──────────────────┐
                  │   runc/crun      │
                  │   (OCI Runtime)  │
                  └──────────────────┘
```

## Core Components

### 1. Container Manager

**Responsibilities:**
- Load and parse Nexus YAML configs
- Create containers from configs
- Manage container lifecycle (create, start, stop, restart, delete)
- Handle container state transitions
- Apply resource limits (CPU, memory, disk I/O)

**Key Features:**
- Idempotent operations
- Graceful degradation
- Automatic restart policies
- Health checking

### 2. Lifecycle Manager

**Responsibilities:**
- Monitor container state
- Detect crashes and restart if needed
- Handle startup/shutdown sequences
- Execute pre-start and post-stop hooks
- Manage container dependencies

**Key Features:**
- Configurable restart policies (always, on-failure, unless-stopped)
- Exponential backoff for failing containers
- Graceful shutdown with timeout
- Cleanup on container removal

### 3. gRPC API Server

**Endpoints:**
```protobuf
service Wings {
  // Container Management
  rpc CreateServer(CreateServerRequest) returns (CreateServerResponse);
  rpc StartServer(ServerRequest) returns (ServerResponse);
  rpc StopServer(StopServerRequest) returns (ServerResponse);
  rpc RestartServer(ServerRequest) returns (ServerResponse);
  rpc DeleteServer(ServerRequest) returns (ServerResponse);
  rpc GetServerState(ServerRequest) returns (ServerStateResponse);

  // Console Access
  rpc SendCommand(CommandRequest) returns (CommandResponse);
  rpc StreamLogs(ServerRequest) returns (stream LogEntry);
  rpc AttachConsole(ServerRequest) returns (stream ConsoleData);

  // Resource Management
  rpc GetStats(ServerRequest) returns (ServerStats);
  rpc UpdateResources(UpdateResourcesRequest) returns (ServerResponse);

  // File Operations
  rpc ReadFile(FileRequest) returns (FileContent);
  rpc WriteFile(WriteFileRequest) returns (FileResponse);
  rpc ListFiles(ListFilesRequest) returns (FileList);
}
```

### 4. Resource Monitor

**Responsibilities:**
- Track CPU usage per container
- Track memory usage (RSS, cache, swap)
- Track network I/O (bytes in/out, packets)
- Track disk I/O (reads, writes, IOPS)
- Enforce resource limits

**Key Features:**
- Real-time metrics collection (1s intervals)
- Cgroup v2 integration
- OOM (Out of Memory) detection
- Automatic throttling when limits exceeded

### 5. Console Streaming

**Responsibilities:**
- Capture container stdout/stderr
- Stream logs to clients via gRPC
- Support console input (stdin)
- Buffer recent logs for late joiners

**Key Features:**
- WebSocket-compatible streaming
- Log rotation and archival
- Filtering by log level
- Tail-follow mode

### 6. Prometheus Metrics

**Exposed Metrics:**
```
# Container metrics
wings_container_state{id, name} - Container state (0=stopped, 1=running, 2=paused)
wings_container_restarts_total{id, name} - Total restart count
wings_container_cpu_usage_seconds{id, name} - CPU time consumed
wings_container_memory_bytes{id, name, type} - Memory usage by type
wings_container_network_bytes{id, name, direction} - Network traffic
wings_container_disk_bytes{id, name, operation} - Disk I/O

# Wings metrics
wings_containers_total - Total containers managed
wings_grpc_requests_total{method, status} - gRPC request count
wings_grpc_request_duration_seconds{method} - Request latency
```

## Technology Stack

### Core Dependencies

```toml
[dependencies]
# Async runtime
tokio = { version = "1.40", features = ["full"] }
tokio-util = "0.7"

# gRPC
tonic = "0.12"
prost = "0.13"

# Containerd client
containerd-client = "0.7"

# Monitoring
prometheus = "0.13"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_yaml = "0.9"

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"

# Error handling
anyhow = "1.0"
thiserror = "1.0"
```

### System Requirements

- **Containerd** 1.7+ (container runtime)
- **Linux** 5.15+ (for cgroup v2, eBPF)
- **Root or CAP_SYS_ADMIN** (for container operations)

## Data Flow

### Starting a Server

```
1. Panel → Wings (gRPC): StartServer(server_id)
2. Wings → Config Loader: Load YAML config
3. Wings → Containerd: Create container spec
4. Containerd → OCI Runtime: Create container
5. Wings → Containerd: Start container
6. Wings → Resource Monitor: Begin tracking
7. Wings → Panel: Return success + container info
```

### Monitoring Flow

```
1. Resource Monitor (every 1s):
   - Read cgroup stats
   - Calculate deltas
   - Update Prometheus metrics

2. Log Streaming:
   - Container stdout/stderr → Wings buffer
   - Wings buffer → gRPC stream → Panel
   - Optional: Write to disk for archival
```

## Security Model

### Container Isolation

1. **Linux Namespaces**
   - PID namespace (isolated process tree)
   - Network namespace (isolated networking)
   - Mount namespace (isolated filesystem)
   - User namespace (UID/GID mapping)
   - IPC namespace (isolated IPC)

2. **Cgroups v2**
   - CPU quota (prevent CPU hogging)
   - Memory limit (prevent OOM)
   - Disk I/O throttling
   - Network bandwidth limits

3. **Seccomp Filters**
   - Block dangerous syscalls
   - Default deny with allowlist
   - Custom profiles per game type

4. **Capabilities Dropping**
   - Start with no capabilities
   - Add only required caps (NET_BIND_SERVICE, etc.)
   - Never allow CAP_SYS_ADMIN in containers

### Network Security

1. **Port Isolation**
   - Each container gets isolated network namespace
   - Port forwarding via iptables/nftables
   - Optional: XDP-based firewall (future)

2. **Rate Limiting**
   - Per-IP connection limits
   - Bandwidth throttling
   - DDoS protection

## File System Layout

```
/var/lib/nexus-wings/
├── servers/
│   ├── {server-id}/
│   │   ├── config.yaml          # Nexus config
│   │   ├── container.json       # Containerd container spec
│   │   ├── data/                # Server files (mounted into container)
│   │   └── logs/                # Archived logs
│   └── ...
├── state/
│   └── containers.db            # Container state (SQLite or JSON)
└── cache/
    └── images/                  # Cached container images
```

## Configuration

Wings daemon config (`/etc/nexus-wings/config.yaml`):

```yaml
# Wings daemon configuration
daemon:
  # gRPC listen address
  grpc_bind: "127.0.0.1:8080"

  # Prometheus metrics
  metrics_bind: "127.0.0.1:9090"

  # Data directory
  data_dir: "/var/lib/nexus-wings"

  # Log level
  log_level: "info"

# Containerd connection
containerd:
  socket: "/run/containerd/containerd.sock"
  namespace: "nexus-panel"

# Resource defaults
resources:
  # Default CPU limit (millicores)
  default_cpu_limit: 1000

  # Default memory limit (MB)
  default_memory_limit: 1024

  # Default disk limit (GB)
  default_disk_limit: 10

# Networking
network:
  # Default network mode
  mode: "bridge"

  # Port range for game servers
  port_range: "25565-25665"

  # Enable IPv6
  ipv6: false

# Monitoring
monitoring:
  # Metrics collection interval
  interval: "1s"

  # Log retention (days)
  log_retention: 7
```

## Error Handling

### Failure Modes

1. **Container Crash**
   - Detect via exit code
   - Log crash reason
   - Apply restart policy
   - Notify panel via webhook

2. **OOM Kill**
   - Detect via cgroup events
   - Log memory usage at time of kill
   - Increase memory limit if configured
   - Restart container

3. **Network Failure**
   - Detect via health check failure
   - Attempt to recreate network namespace
   - Restart container if needed

4. **Disk Full**
   - Monitor disk usage
   - Stop containers when threshold reached (95%)
   - Send alert to panel

## Performance Targets

- **Container Start Time**: < 2 seconds (cold start)
- **Container Stop Time**: < 5 seconds (graceful shutdown)
- **gRPC Request Latency**: < 50ms (p99)
- **Memory Overhead**: < 50MB per container
- **CPU Overhead**: < 2% per container
- **Max Containers**: 1000+ per node

## Development Phases

### Phase 1: MVP (Week 1-2)
- ✅ Basic container lifecycle (create, start, stop, delete)
- ✅ Load Nexus YAML configs
- ✅ Containerd integration
- ✅ Simple gRPC API (start/stop/status)
- ✅ Console output streaming

### Phase 2: Monitoring (Week 3)
- Resource tracking (CPU, memory, network, disk)
- Prometheus metrics
- Health checking
- Restart policies

### Phase 3: Advanced Features (Week 4+)
- File operations (read/write/list)
- Console input (stdin)
- XDP firewall integration
- Log archival and rotation

### Phase 4: Optimization (Week 5+)
- Performance tuning
- Load testing
- Memory optimizations
- Parallel container operations

## Testing Strategy

1. **Unit Tests**
   - Config parsing
   - State machine logic
   - Resource calculations

2. **Integration Tests**
   - Containerd operations
   - gRPC API endpoints
   - Metrics collection

3. **End-to-End Tests**
   - Full server lifecycle
   - Load testing (100+ containers)
   - Failure recovery

4. **Chaos Testing**
   - Random container kills
   - Network failures
   - Disk full scenarios

## Future Enhancements

- **eBPF/XDP Firewall**: Kernel-level packet filtering for DDoS protection
- **Live Migration**: Move running containers between nodes
- **Snapshots**: Create/restore container snapshots
- **GPU Support**: Pass-through GPU for game servers
- **Multi-Node**: Cluster mode with load balancing

---

**Status**: 🎯 Ready to implement Phase 1 MVP
**Next Step**: Create Wings crate and set up dependencies

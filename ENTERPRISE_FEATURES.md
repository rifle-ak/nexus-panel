# Nexus Node - Enterprise Features

This document describes the enterprise-grade features available in Nexus Node for production deployments.

## Overview

Nexus Node includes comprehensive enterprise features designed for:

- **DDoS Protection**: Cloudflare Spectrum integration and XDP/eBPF firewall
- **Security**: Authentication, authorization, TLS/mTLS, input validation
- **Reliability**: Circuit breakers, rate limiting, graceful degradation
- **Observability**: Audit logging, distributed tracing, correlation IDs
- **Operations**: Configuration hot reload, feature flags, health checks

All enterprise features are **disabled by default** for backward compatibility and can be enabled via environment variables.

## Table of Contents

### Key Features
1. [Cloudflare Integration](#cloudflare-integration) - DDoS protection and Spectrum proxy
2. [XDP Firewall](#xdp-firewall) - Kernel-level packet filtering

### Security & Operations
3. [Authentication](#authentication)
4. [Rate Limiting](#rate-limiting)
5. [TLS/mTLS](#tlsmtls)
6. [Audit Logging](#audit-logging)
7. [Circuit Breakers](#circuit-breakers)
8. [Request Tracing](#request-tracing)
9. [Input Validation](#input-validation)
10. [Graceful Degradation](#graceful-degradation)
11. [Configuration Hot Reload](#configuration-hot-reload)
12. [Environment Variables Reference](#environment-variables-reference)

---

## Cloudflare Integration

**Enterprise-grade DDoS protection for game servers using Cloudflare Spectrum.**

Cloudflare Spectrum provides Layer 4 reverse proxy capabilities, protecting TCP/UDP game server traffic from DDoS attacks while maintaining low latency for players.

### Key Benefits

- **DDoS Mitigation**: Enterprise-grade protection against volumetric and protocol attacks
- **Low Latency**: Anycast network with 300+ data centers worldwide
- **No IP Exposure**: Origin server IP hidden behind Cloudflare's network
- **Automatic Failover**: Built-in health checks and failover capabilities
- **UDP Support**: Full support for UDP-based game protocols

### Supported Games

| Game | Protocol | Default Port |
|------|----------|--------------|
| Minecraft | TCP | 25565 |
| Rust | UDP | 28015 |
| ARK: Survival | UDP | 7777 |
| Valheim | UDP | 2456 |
| Counter-Strike 2 | UDP | 27015 |
| Team Fortress 2 | UDP | 27015 |
| Garry's Mod | UDP | 27015 |
| Custom | TCP/UDP | Any |

### Configuration

```bash
# Enable Cloudflare integration
CLOUDFLARE_ENABLED=true

# Cloudflare API credentials
CLOUDFLARE_API_TOKEN=your-api-token
CLOUDFLARE_ZONE_ID=your-zone-id

# Domain configuration
CLOUDFLARE_DOMAIN=servers.example.com

# Optional: Custom API URL (for enterprise accounts)
CLOUDFLARE_API_URL=https://api.cloudflare.com/client/v4

# Spectrum settings
CLOUDFLARE_SPECTRUM_IP_FIREWALL=true
CLOUDFLARE_SPECTRUM_PROXY_PROTOCOL=off
CLOUDFLARE_SPECTRUM_EDGE_IPS=dynamic
```

### Quick Setup

```rust
use nexus_node::{CloudflareClient, CloudflareConfig};

// Initialize client
let config = CloudflareConfig::from_env()?;
let client = CloudflareClient::new(config);

// One-call game server setup with DDoS protection
let result = client.setup_game_server(
    "my-rust-server",           // Server name
    "rust",                     // Game type
    "192.168.1.100",           // Origin IP
    28015,                      // Port
).await?;

println!("Players connect to: {}", result.dns_record.name);
// Output: Players connect to: my-rust-server.servers.example.com
```

### Manual Spectrum App Creation

```rust
// Create Spectrum app for custom game/port
let app = client.create_spectrum_app(
    "custom-game",              // Name
    "tcp",                      // Protocol (tcp/udp)
    12345,                      // Edge port (Cloudflare-facing)
    "192.168.1.100",           // Origin IP
    12345,                      // Origin port
).await?;
```

### DNS Record Management

```rust
// Create A record pointing to Spectrum
let record = client.create_dns_record(
    "A",                        // Record type
    "game.example.com",         // Name
    "spectrum-ip",              // Content (Cloudflare provides)
    true,                       // Proxied through Cloudflare
    Some(300),                  // TTL (ignored when proxied)
).await?;

// Create SRV record for Minecraft
let srv = client.create_srv_record(
    "minecraft",                // Service name
    "tcp",                      // Protocol
    "mc.example.com",          // Target
    25565,                      // Port
    1,                          // Priority
    1,                          // Weight
).await?;
```

### Complete Server Teardown

```rust
// Remove all Cloudflare resources for a server
client.teardown_game_server("my-rust-server").await?;
```

### API Token Permissions

Create a Cloudflare API token with these permissions:

| Permission | Access Level |
|------------|--------------|
| Zone.Spectrum | Edit |
| Zone.DNS | Edit |
| Zone.Zone | Read |

---

## XDP Firewall

**High-performance kernel-level packet filtering using XDP/eBPF technology.**

XDP (eXpress Data Path) provides the fastest possible packet filtering by processing packets at the earliest point in the Linux network stack - before the kernel allocates any memory for the packet.

### Performance

| Metric | XDP | iptables |
|--------|-----|----------|
| Packets/sec | 20M+ | ~3M |
| Latency | <1μs | ~10μs |
| CPU Usage | Minimal | High |
| Memory | Zero-copy | Per-packet allocation |

### Key Features

- **Kernel-Level Filtering**: Packets filtered before reaching userspace
- **Game-Specific Profiles**: Pre-configured protections for popular games
- **Rate Limiting**: Per-IP and global rate limiting
- **IP Management**: Allowlists, blocklists, and auto-blocking
- **Zero Downtime**: Rules updated without service restart
- **Statistics**: Real-time packet and byte counters

### Configuration

```bash
# Enable XDP firewall
XDP_FIREWALL_ENABLED=true

# Network interface to attach XDP program
XDP_INTERFACE=eth0

# XDP mode: native (fastest), offload (NIC), generic (fallback)
XDP_MODE=native

# Rate limiting
XDP_PER_IP_RATE_LIMIT=10000
XDP_GLOBAL_RATE_LIMIT=1000000
XDP_BURST_SIZE=1000

# Auto-blocking
XDP_AUTO_BLOCK_ENABLED=true
XDP_AUTO_BLOCK_THRESHOLD=50000
XDP_AUTO_BLOCK_DURATION=300

# Game protection (enabled by default)
XDP_GAME_PROTECTION_ENABLED=true
```

### Quick Setup

```rust
use nexus_node::{XdpFirewall, FirewallConfig, GameType};

// Initialize firewall
let config = FirewallConfig::from_env()?;
let firewall = XdpFirewall::new(config).await?;

// Apply game-specific protection profile
firewall.apply_game_profile(GameType::Rust, 28015).await?;
```

### Game Protection Profiles

Each game profile includes optimized rules for that game's protocol:

```rust
// Available game types
GameType::Minecraft     // TCP 25565 - Block invalid packets, query floods
GameType::Rust          // UDP 28015 - RCON protection, query rate limits
GameType::Ark           // UDP 7777  - Query flood protection
GameType::Valheim       // UDP 2456  - Steam query protection
GameType::Csgo          // UDP 27015 - A2S query rate limiting
GameType::Tf2           // UDP 27015 - Similar to CS:GO
GameType::GarrysMod     // UDP 27015 - Lua exploit protection
GameType::Custom        // Any port  - Basic DDoS protection
```

### IP Management

```rust
// Block a malicious IP
firewall.block_ip("192.168.1.50".parse()?).await?;

// Allow a trusted IP (bypasses rate limits)
firewall.allow_ip("10.0.0.1".parse()?).await?;

// Rate limit a specific IP
firewall.rate_limit_ip(
    "192.168.1.100".parse()?,
    1000,  // packets per second
).await?;

// Remove an IP from blocklist
firewall.unblock_ip("192.168.1.50".parse()?).await?;

// Get current blocklist
let blocked = firewall.get_blocked_ips().await?;
```

### Custom Rules

```rust
use nexus_node::FirewallRule;

// Block specific port
let rule = FirewallRule::new()
    .destination_port(22)
    .action(Action::Drop)
    .build();
firewall.add_rule(rule).await?;

// Allow specific subnet
let rule = FirewallRule::new()
    .source_cidr("10.0.0.0/8".parse()?)
    .action(Action::Allow)
    .build();
firewall.add_rule(rule).await?;

// Rate limit UDP traffic on port 27015
let rule = FirewallRule::new()
    .protocol(Protocol::Udp)
    .destination_port(27015)
    .rate_limit(5000)  // pps
    .action(Action::RateLimit)
    .build();
firewall.add_rule(rule).await?;
```

### Statistics

```rust
// Get firewall statistics
let stats = firewall.get_stats().await?;
println!("Packets processed: {}", stats.packets_processed);
println!("Packets dropped: {}", stats.packets_dropped);
println!("Bytes processed: {}", stats.bytes_processed);

// Get per-IP statistics
let ip_stats = firewall.get_ip_stats("192.168.1.100".parse()?).await?;
println!("Packets from IP: {}", ip_stats.packet_count);
```

### XDP Modes

| Mode | Performance | Requirements |
|------|-------------|--------------|
| `native` | Fastest | Driver support required |
| `offload` | Hardware | NIC with XDP offload |
| `generic` | Slower | Any Linux kernel 4.8+ |

### Kernel Requirements

- Linux kernel 4.8+ for generic XDP
- Linux kernel 4.15+ for native XDP
- `CAP_NET_ADMIN` and `CAP_BPF` capabilities
- BPF filesystem mounted at `/sys/fs/bpf`

---

---

## Authentication

Nexus Node supports multiple authentication methods for securing gRPC endpoints.

### Supported Methods

1. **API Key Authentication** - Simple key-based authentication via `X-API-Key` header
2. **JWT Bearer Token** - Full JWT authentication with claims support
3. **mTLS Client Certificates** - Mutual TLS for service-to-service authentication

### Configuration

```bash
# Enable authentication
AUTH_ENABLED=true

# API Keys (comma-separated, will be hashed internally)
AUTH_API_KEYS=your-api-key-1,your-api-key-2

# JWT Configuration
AUTH_JWT_SECRET=your-jwt-secret-key
AUTH_JWT_ISSUER=nexus-panel
AUTH_JWT_AUDIENCE=nexus-node

# Token leeway for clock skew (seconds)
AUTH_TOKEN_LEEWAY=60

# Methods that bypass authentication (comma-separated)
AUTH_BYPASS_METHODS=/nexus.node.v1.NodeService/HealthCheck
```

### JWT Token Structure

```json
{
  "sub": "user-id",
  "iss": "nexus-panel",
  "aud": "nexus-node",
  "exp": 1234567890,
  "iat": 1234567800,
  "roles": ["admin", "operator"],
  "scopes": ["containers:read", "containers:write"],
  "tenant_id": "tenant-123",
  "node_id": "node-specific"
}
```

### Usage Example

```bash
# With API Key
grpcurl -H "X-API-Key: your-api-key" localhost:8080 nexus.node.v1.NodeService/ListContainers

# With JWT Bearer Token
grpcurl -H "Authorization: Bearer eyJhbG..." localhost:8080 nexus.node.v1.NodeService/ListContainers
```

---

## Rate Limiting

Protect your services from abuse with configurable rate limiting.

### Features

- **Global Rate Limiting**: Limit total requests per second across all clients
- **Per-Client Limiting**: Individual rate limits based on IP or API key
- **Per-Method Limiting**: Custom limits for specific gRPC methods
- **Burst Handling**: Token bucket algorithm for handling traffic spikes
- **Concurrent Request Limits**: Prevent single clients from monopolizing resources
- **Automatic Blocking**: Temporarily block clients with repeated violations

### Configuration

```bash
# Enable rate limiting
RATE_LIMIT_ENABLED=true

# Global requests per second
RATE_LIMIT_GLOBAL_RPS=10000

# Burst size (token bucket)
RATE_LIMIT_BURST_SIZE=1000

# Per-client requests per second
RATE_LIMIT_PER_CLIENT_RPS=100

# Maximum concurrent requests per client
RATE_LIMIT_MAX_CONCURRENT=20
```

### Response Headers

When rate limited, clients receive:
- HTTP Status: `429 Too Many Requests`
- Header: `Retry-After: <seconds>`

---

## TLS/mTLS

Secure all communications with TLS and optional mutual TLS for client verification.

### Configuration

```bash
# Enable TLS
TLS_ENABLED=true

# Server certificate and key
TLS_CERT_PATH=/etc/nexus/certs/server.crt
TLS_KEY_PATH=/etc/nexus/certs/server.key

# CA certificate for client verification (mTLS)
TLS_CA_CERT_PATH=/etc/nexus/certs/ca.crt

# Require client certificates
TLS_REQUIRE_CLIENT_CERT=true

# Minimum TLS version (1.2 or 1.3)
TLS_MIN_VERSION=1.3

# Certificate reload interval (seconds, 0 = disabled)
TLS_RELOAD_INTERVAL=3600
```

### Certificate Generation (Development)

```bash
# Generate CA
openssl genrsa -out ca.key 4096
openssl req -new -x509 -days 365 -key ca.key -out ca.crt -subj "/CN=Nexus CA"

# Generate server certificate
openssl genrsa -out server.key 4096
openssl req -new -key server.key -out server.csr -subj "/CN=nexus-node"
openssl x509 -req -days 365 -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt

# Generate client certificate (for mTLS)
openssl genrsa -out client.key 4096
openssl req -new -key client.key -out client.csr -subj "/CN=nexus-client"
openssl x509 -req -days 365 -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt
```

---

## Audit Logging

Comprehensive audit trail for security and compliance requirements.

### Logged Events

| Category | Events |
|----------|--------|
| **Authentication** | Login success/failure, token refresh, session start/end |
| **Authorization** | Access granted/denied, permission elevation |
| **Container Operations** | Create, start, stop, restart, delete, failures |
| **Configuration** | Config loaded, changed, reloaded |
| **Administrative** | Node start/stop, health changes, secrets accessed |
| **Security** | Rate limit exceeded, suspicious activity, policy violations |

### Configuration

```bash
# Enable audit logging
AUDIT_ENABLED=true

# Log file path (optional)
AUDIT_LOG_FILE=/var/log/nexus-node/audit.log

# Also log to stdout (JSON format)
AUDIT_STDOUT=true

# Minimum severity to log (debug, info, notice, warning, error, critical)
AUDIT_MIN_SEVERITY=info

# Node ID for log correlation
NODE_ID=node-1
```

### Log Format

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2024-01-15T10:30:00Z",
  "event_type": "CONTAINER_CREATED",
  "severity": "info",
  "actor": {
    "actor_type": "user",
    "id": "admin@example.com",
    "auth_method": "jwt",
    "roles": ["admin"]
  },
  "target": {
    "target_type": "container",
    "id": "rust-server-1",
    "name": "Rust Game Server"
  },
  "action": "create_container",
  "outcome": "success",
  "correlation_id": "req-12345",
  "source_ip": "192.168.1.100",
  "node_id": "node-1",
  "checksum": "a1b2c3d4..."
}
```

### Tamper Detection

Audit logs include checksums that chain together, making tampering detectable:
- Each log entry includes a `checksum` of the entry content
- Each entry references the `prev_checksum` of the previous entry
- Breaking the chain indicates potential tampering

---

## Circuit Breakers

Prevent cascading failures with automatic circuit breaker protection.

### States

1. **Closed** - Normal operation, requests pass through
2. **Open** - Circuit tripped, requests fail fast
3. **Half-Open** - Testing if service has recovered

### Configuration

```bash
# Enable circuit breakers
CIRCUIT_BREAKER_ENABLED=true

# Number of failures before opening circuit
CIRCUIT_BREAKER_FAILURE_THRESHOLD=5

# Time to wait before testing recovery (seconds)
CIRCUIT_BREAKER_RECOVERY_TIMEOUT=30

# Successful calls needed to close circuit
CIRCUIT_BREAKER_SUCCESS_THRESHOLD=3
```

### Behavior

1. Failures are tracked within a rolling time window
2. When failures exceed threshold, circuit opens
3. While open, requests fail immediately without calling backend
4. After recovery timeout, circuit enters half-open state
5. Successful calls in half-open state close the circuit
6. Any failure in half-open state reopens the circuit

---

## Request Tracing

Distributed tracing support with correlation IDs for debugging and monitoring.

### Headers

| Header | Description |
|--------|-------------|
| `X-Request-ID` | Unique identifier for the request |
| `X-Correlation-ID` | Cross-service correlation ID |
| `traceparent` | W3C Trace Context propagation |

### Configuration

```bash
# Enable request tracing
TRACING_ENABLED=true

# Service name for tracing
SERVICE_NAME=nexus-node

# OTLP endpoint for trace export (optional)
OTLP_ENDPOINT=http://jaeger:4317

# Sample rate (0.0 - 1.0)
TRACING_SAMPLE_RATE=1.0

# Log request headers
TRACING_LOG_HEADERS=true
```

### Usage

```bash
# Requests automatically get IDs assigned
grpcurl localhost:8080 nexus.node.v1.NodeService/ListContainers

# Or provide your own correlation ID
grpcurl -H "X-Correlation-ID: my-trace-123" localhost:8080 nexus.node.v1.NodeService/ListContainers
```

Response headers will include:
```
x-request-id: 550e8400-e29b-41d4-a716-446655440000
x-correlation-id: my-trace-123
```

---

## Input Validation

Security-focused input validation to prevent injection attacks.

### Validated Fields

- Container IDs and names
- Configuration YAML
- Port numbers
- File paths
- Environment variables
- Timeout values

### Security Checks

- Command injection patterns (`rm -rf`, `curl | bash`, etc.)
- Path traversal attempts (`../`, `/etc/passwd`)
- Null byte injection
- Control character injection
- Overly permissive commands (`chmod 777`)

### Example Rejections

```yaml
# These will be rejected:
command: rm -rf /           # Dangerous command
path: ../../../etc/passwd   # Path traversal
env: VAR=`whoami`          # Command substitution
```

---

## Graceful Degradation

Handle high load and failures gracefully without complete service interruption.

### Features

#### Load Shedding

Automatically drop requests under high load:

```bash
# Maximum concurrent requests before shedding
RATE_LIMIT_MAX_CONCURRENT=1000
```

#### Feature Flags

Runtime feature toggling without restart:

```rust
// Programmatic usage
let flags = FeatureFlags::new();
flags.register("new_feature", false);

// Enable at runtime
flags.enable("new_feature");

// Check before executing
if flags.is_enabled("new_feature") {
    // New code path
}
```

#### Bulkhead Isolation

Prevent single components from consuming all resources:

```rust
let db_bulkhead = Bulkhead::new("database", 50);
let cache_bulkhead = Bulkhead::new("cache", 100);

// Each component has its own connection limit
let permit = db_bulkhead.try_acquire()?;
```

#### Graceful Shutdown

Handle shutdown signals properly:

1. Stop accepting new requests
2. Wait for in-flight requests to complete
3. Timeout after configurable duration
4. Log final audit events

---

## Configuration Hot Reload

Update configuration without service restart.

### Supported Configurations

- Rate limit settings
- Feature flags
- Circuit breaker parameters
- Log levels (via RUST_LOG)

### File-Based Configuration

```rust
// Watch configuration file for changes
let watcher = ConfigWatcher::load_from_file("/etc/nexus/config.yaml")?;
watcher.start_watching()?;

// Subscribe to changes
let mut rx = watcher.subscribe();
tokio::spawn(async move {
    while rx.recv().await.is_ok() {
        println!("Configuration reloaded!");
    }
});
```

### Environment-Based Configuration

```rust
// Reload from environment
let mut env_config = EnvConfig::from_env("NEXUS");
env_config.reload();
```

---

## Environment Variables Reference

### Core Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `GRPC_BIND` | `127.0.0.1:8080` | gRPC server bind address |
| `METRICS_BIND` | `127.0.0.1:9090` | Metrics HTTP server address |
| `NODE_ID` | hostname | Node identifier |
| `DATA_DIR` | `/var/lib/nexus-node` | Data directory |
| `LOG_FORMAT` | `text` | Log format (`text` or `json`) |

### Authentication

| Variable | Default | Description |
|----------|---------|-------------|
| `AUTH_ENABLED` | `false` | Enable authentication |
| `AUTH_API_KEYS` | - | Comma-separated API keys |
| `AUTH_JWT_SECRET` | - | JWT signing secret |
| `AUTH_JWT_ISSUER` | - | Expected JWT issuer |
| `AUTH_JWT_AUDIENCE` | - | Expected JWT audience |
| `AUTH_TOKEN_LEEWAY` | `60` | Token validation leeway (seconds) |

### Rate Limiting

| Variable | Default | Description |
|----------|---------|-------------|
| `RATE_LIMIT_ENABLED` | `true` | Enable rate limiting |
| `RATE_LIMIT_GLOBAL_RPS` | `10000` | Global requests per second |
| `RATE_LIMIT_BURST_SIZE` | `1000` | Burst size |
| `RATE_LIMIT_PER_CLIENT_RPS` | `100` | Per-client requests per second |
| `RATE_LIMIT_MAX_CONCURRENT` | `20` | Max concurrent per client |

### TLS

| Variable | Default | Description |
|----------|---------|-------------|
| `TLS_ENABLED` | `false` | Enable TLS |
| `TLS_CERT_PATH` | - | Server certificate path |
| `TLS_KEY_PATH` | - | Server private key path |
| `TLS_CA_CERT_PATH` | - | CA certificate for mTLS |
| `TLS_REQUIRE_CLIENT_CERT` | `false` | Require client certificates |
| `TLS_MIN_VERSION` | `1.2` | Minimum TLS version |

### Audit Logging

| Variable | Default | Description |
|----------|---------|-------------|
| `AUDIT_ENABLED` | `false` | Enable audit logging |
| `AUDIT_LOG_FILE` | - | Audit log file path |
| `AUDIT_STDOUT` | `true` | Log to stdout |
| `AUDIT_MIN_SEVERITY` | `info` | Minimum severity |

### Circuit Breakers

| Variable | Default | Description |
|----------|---------|-------------|
| `CIRCUIT_BREAKER_ENABLED` | `true` | Enable circuit breakers |
| `CIRCUIT_BREAKER_FAILURE_THRESHOLD` | `5` | Failures before opening |
| `CIRCUIT_BREAKER_RECOVERY_TIMEOUT` | `30` | Recovery timeout (seconds) |
| `CIRCUIT_BREAKER_SUCCESS_THRESHOLD` | `3` | Successes to close |

### Tracing

| Variable | Default | Description |
|----------|---------|-------------|
| `TRACING_ENABLED` | `true` | Enable request tracing |
| `SERVICE_NAME` | `nexus-node` | Service name for traces |
| `OTLP_ENDPOINT` | - | OpenTelemetry endpoint |
| `TRACING_SAMPLE_RATE` | `1.0` | Sample rate (0.0-1.0) |

### Cloudflare Integration

| Variable | Default | Description |
|----------|---------|-------------|
| `CLOUDFLARE_ENABLED` | `false` | Enable Cloudflare integration |
| `CLOUDFLARE_API_TOKEN` | - | Cloudflare API token |
| `CLOUDFLARE_ZONE_ID` | - | Cloudflare zone ID |
| `CLOUDFLARE_DOMAIN` | - | Base domain for game servers |
| `CLOUDFLARE_API_URL` | `https://api.cloudflare.com/client/v4` | API endpoint |
| `CLOUDFLARE_SPECTRUM_IP_FIREWALL` | `true` | Enable IP firewall on Spectrum |
| `CLOUDFLARE_SPECTRUM_PROXY_PROTOCOL` | `off` | Proxy protocol setting |

### XDP Firewall

| Variable | Default | Description |
|----------|---------|-------------|
| `XDP_FIREWALL_ENABLED` | `false` | Enable XDP firewall |
| `XDP_INTERFACE` | `eth0` | Network interface |
| `XDP_MODE` | `native` | XDP mode (native/offload/generic) |
| `XDP_PER_IP_RATE_LIMIT` | `10000` | Per-IP packet rate limit |
| `XDP_GLOBAL_RATE_LIMIT` | `1000000` | Global packet rate limit |
| `XDP_BURST_SIZE` | `1000` | Burst allowance |
| `XDP_AUTO_BLOCK_ENABLED` | `true` | Auto-block abusive IPs |
| `XDP_AUTO_BLOCK_THRESHOLD` | `50000` | Packets/sec to trigger block |
| `XDP_AUTO_BLOCK_DURATION` | `300` | Block duration (seconds) |
| `XDP_GAME_PROTECTION_ENABLED` | `true` | Enable game-specific rules |

---

## Production Checklist

Before deploying to production, ensure:

### DDoS Protection

- [ ] `CLOUDFLARE_ENABLED=true` with valid API token
- [ ] Cloudflare Spectrum configured for game server ports
- [ ] `XDP_FIREWALL_ENABLED=true` for kernel-level protection
- [ ] `XDP_MODE=native` for maximum performance (if supported)
- [ ] Game-specific protection profiles applied
- [ ] Auto-blocking configured with appropriate thresholds
- [ ] IP allowlists configured for trusted sources

### Security

- [ ] `AUTH_ENABLED=true` with strong API keys or JWT
- [ ] `TLS_ENABLED=true` with valid certificates
- [ ] `TLS_MIN_VERSION=1.3` for modern security
- [ ] `TLS_REQUIRE_CLIENT_CERT=true` for service-to-service auth
- [ ] API keys rotated regularly
- [ ] JWT secrets stored securely (not in environment)

### Reliability

- [ ] `RATE_LIMIT_ENABLED=true` with appropriate limits
- [ ] `CIRCUIT_BREAKER_ENABLED=true`
- [ ] Health checks monitored and alerted
- [ ] Graceful shutdown configured

### Observability

- [ ] `AUDIT_ENABLED=true` with secure log storage
- [ ] `TRACING_ENABLED=true` with OTLP endpoint
- [ ] `LOG_FORMAT=json` for structured logging
- [ ] Metrics scraped by Prometheus
- [ ] Dashboards configured in Grafana
- [ ] XDP firewall statistics monitored

### Operations

- [ ] Configuration backed up
- [ ] TLS certificates auto-renewed
- [ ] Audit logs archived and retained
- [ ] Runbooks documented
- [ ] Incident response procedures in place
- [ ] Cloudflare API token rotation scheduled
- [ ] XDP rules backup and recovery tested

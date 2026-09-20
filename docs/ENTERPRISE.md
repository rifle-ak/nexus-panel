# Enterprise Features

Advanced features for production deployments.

## Cloudflare Spectrum Integration

Upstream L4 mitigation through Cloudflare Spectrum, in front of the node's
own firewall (below). The client exists in `cloudflare.rs`; it is not yet
called by provisioning, so Spectrum applications are created by hand.

### Configuration

```bash
export CLOUDFLARE_ENABLED=true
export CLOUDFLARE_API_TOKEN=your-token
export CLOUDFLARE_ZONE_ID=your-zone-id
export CLOUDFLARE_DOMAIN=example.com
export CLOUDFLARE_SPECTRUM_ENABLED=true
```

### Features

- L4 DDoS mitigation
- TCP/UDP proxy support
- Automatic failover
- Real-time analytics

## Firewall and DDoS Protection

Every node runs an nftables firewall (`NEXUS_FIREWALL=auto`, on whenever
`nft` is installed and the node runs as root; the installer sets both up).
It protects all game ports at once and gives each server its own rules.

### Node-wide protection

| Setting | Default | What it does |
|---------|---------|--------------|
| `NEXUS_FIREWALL` | `auto` | `auto`, `on` (missing `nft` is a startup error), `off` |
| `NEXUS_FIREWALL_SYN_PER_SOURCE` | `50` | New TCP connections per second one address may open to game ports |
| `NEXUS_FIREWALL_SYN_GLOBAL` | `20000` | New TCP connections per second across all game ports |
| `NEXUS_FIREWALL_UDP_PER_SOURCE` | `2000` | UDP packets per second one address may send to game ports |
| `NEXUS_FIREWALL_TRUSTED` | empty | Comma-separated CIDRs never filtered (your office, monitoring) |
| `NEXUS_FIREWALL_SYSCTL` | `on` | Apply SYN cookies, SYN backlog and conntrack sysctls |

The operator's blocklist and trusted list live on the **Security** page and
under `/api/v1/firewall`. A block can carry a TTL; it lifts on its own.

### Per-server rules

Rules in a blueprint's `security.firewall_rules` apply to that server's
ports while it runs, and can be edited on the server's **Firewall** tab (or
`PUT /api/v1/containers/:id/firewall`) by the operator or by the customer
who owns it. Counters per rule show what each one dropped.

```yaml
security:
  firewall_rules:
    # New TCP connections per second (and UDP packets per second)
    - type: connection_rate
      name: rate_limit
      limit: 100/s
      action: drop

    # Drop oversized UDP (anti-amplification)
    - type: packet_size
      name: size_limit
      max_size: 1500
      action: drop

    # Always let a network in, ahead of the rate limits
    - type: allow_cidr
      name: office
      cidr: 203.0.113.0/24

    # Ban an address or network from this server only
    - type: block_cidr
      name: griefer
      cidr: 198.51.100.7
```

`action` is `drop` or `reject`; rates accept `100/s`, `100/second`,
or `6000/m` (converted to a per-second rate).

## Authentication

### API Key Authentication

```rust
// Generate API key
let key = auth::generate_api_key();

// Validate in middleware
auth::validate_api_key(&request)?;
```

### JWT Authentication

```rust
// Generate JWT
let token = auth::create_jwt(&user_id, Duration::hours(24))?;

// Validate
let claims = auth::validate_jwt(&token)?;
```

### mTLS

Mutual TLS for node-to-panel communication.

```bash
# Environment variables
TLS_ENABLED=true
TLS_CERT_PATH=/path/to/server.crt
TLS_KEY_PATH=/path/to/server.key
TLS_CA_CERT_PATH=/path/to/ca.crt
TLS_REQUIRE_CLIENT_CERT=true
TLS_MIN_VERSION=1.3
```

## Rate Limiting

Per-client request rate limiting.

### Configuration

```bash
RATE_LIMIT_ENABLED=true
RATE_LIMIT_GLOBAL_RPS=10000
RATE_LIMIT_BURST_SIZE=1000
RATE_LIMIT_PER_CLIENT_RPS=100
RATE_LIMIT_MAX_CONCURRENT=20
```

### Response Headers

```
x-ratelimit-limit: 1000
x-ratelimit-remaining: 950
x-ratelimit-reset: 1705312800
```

### Exceeded Response

```
Status: 429 Too Many Requests
Retry-After: 60
```

## Circuit Breakers

Prevent cascade failures with automatic circuit breaking.

### States

| State | Description |
|-------|-------------|
| Closed | Normal operation |
| Open | Failing, requests rejected |
| Half-Open | Testing recovery |

### Configuration

```rust
CircuitBreaker::new()
    .failure_threshold(5)
    .success_threshold(3)
    .timeout(Duration::from_secs(30))
```

## Request Tracing

Distributed tracing for debugging and monitoring.

### Trace Headers

```
x-trace-id: abc123
x-span-id: def456
x-parent-span-id: ghi789
```

### Integration

Compatible with:
- Jaeger
- Zipkin
- OpenTelemetry

## Audit Logging

Complete audit trail of all operations.

### Log Format

```json
{
  "timestamp": "2024-01-15T10:00:00Z",
  "event": "container.start",
  "user_id": "user-123",
  "container_id": "minecraft-1",
  "ip_address": "192.168.1.50",
  "success": true,
  "duration_ms": 150
}
```

### Events Logged

- Container operations (create, start, stop, delete)
- File operations (read, write, delete)
- Authentication attempts
- Configuration changes
- Backup operations

## Input Validation

Comprehensive request validation.

### Validated Fields

- Container IDs (alphanumeric, hyphens)
- File paths (no traversal)
- YAML configs (schema validation)
- Port numbers (valid ranges)
- Resource limits (reasonable bounds)

## Graceful Degradation

Automatic fallback when services fail.

### Containerd Unavailable

- Mock runtime for development
- Cached container state
- Graceful error messages

### Network Failures

- Retry with exponential backoff
- Cached marketplace data
- Offline operation support

## Configuration Hot Reload

Update configuration without restart.

### Supported Changes

- Rate limits
- Authentication settings
- Logging levels
- Feature flags

### Signal

```bash
# Trigger reload
kill -SIGHUP $(pidof nexus-node)
```

## Secrets Management

Secure handling of sensitive data.

### Environment Variables

```bash
# Never log these
RCON_PASSWORD=secret
API_KEY=secret
JWT_SECRET=secret
```

### Detection

Variables containing these patterns are marked as secrets:
- `password`
- `secret`
- `token`
- `key`
- `api_key`

## Health Checks

Comprehensive health monitoring.

### HTTP Endpoint

```bash
# Simple health (metrics server)
curl http://localhost:9090/health
# Returns: "OK" (200) or error status
```

### gRPC Endpoint

```bash
# Detailed health via gRPC
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/HealthCheck
```

### Checks Performed

- Containerd connectivity
- Disk space available
- Memory available

## Prometheus Metrics

Full observability via Prometheus.

### Container Metrics

```
nexus_node_container_state{id, name}
nexus_node_container_restarts_total{id, name}
nexus_node_container_cpu_usage_seconds{id, name}
nexus_node_container_memory_bytes{id, name, type}
nexus_node_container_network_bytes{id, name, direction}
nexus_node_container_disk_bytes{id, name, operation}
```

### Node Metrics

```
nexus_node_containers_total
nexus_node_containers_running
nexus_node_grpc_requests_total{method, status}
nexus_node_grpc_request_duration_seconds{method}
nexus_node_health_check_status
```

## Environment Variables Reference

| Variable | Description | Default |
|----------|-------------|---------|
| **Core** | | |
| `GRPC_BIND` | gRPC listen address | `127.0.0.1:8080` |
| `METRICS_BIND` | Metrics listen address | `127.0.0.1:9090` |
| `CONTAINERD_SOCKET` | Containerd socket path | `/run/containerd/containerd.sock` |
| `CONTAINERD_NAMESPACE` | Containerd namespace | `nexus-panel` |
| `DATA_DIR` | Data directory | `/var/lib/nexus-node` |
| `NODE_ID` | Node identifier | hostname or `node-1` |
| `LOG_FORMAT` | Log format (`text` or `json`) | `text` |
| `RUST_LOG` | Log level filter | `nexus_node=info` |
| **Health Checks** | | |
| `MIN_DISK_SPACE_BYTES` | Min free disk space | `1073741824` (1GB) |
| `MIN_MEMORY_BYTES` | Min free memory | `536870912` (512MB) |
| **TLS** | | |
| `TLS_ENABLED` | Enable TLS | `false` |
| `TLS_CERT_PATH` | Server certificate path | - |
| `TLS_KEY_PATH` | Server private key path | - |
| `TLS_CA_CERT_PATH` | CA cert for client verification (mTLS) | - |
| `TLS_REQUIRE_CLIENT_CERT` | Require client certs | `false` |
| `TLS_MIN_VERSION` | Minimum TLS version (`1.2`/`1.3`) | `1.2` |
| `TLS_RELOAD_INTERVAL` | Cert reload interval (seconds) | `0` (disabled) |
| **Authentication** | | |
| `AUTH_ENABLED` | Enable authentication | `false` |
| `AUTH_API_KEYS` | Comma-separated API keys | - |
| `AUTH_JWT_SECRET` | JWT HMAC secret | - |
| `AUTH_JWT_PUBLIC_KEY` | JWT public key (RSA/EC) | - |
| `AUTH_JWT_ISSUER` | Expected JWT issuer | - |
| `AUTH_JWT_AUDIENCE` | Expected JWT audience | - |
| `AUTH_TOKEN_LEEWAY` | JWT clock skew tolerance (seconds) | `60` |
| `AUTH_BYPASS_METHODS` | Comma-separated methods to skip auth | - |
| **Rate Limiting** | | |
| `RATE_LIMIT_ENABLED` | Enable rate limiting | `true` |
| `RATE_LIMIT_GLOBAL_RPS` | Global requests per second | `10000` |
| `RATE_LIMIT_BURST_SIZE` | Burst size | `1000` |
| `RATE_LIMIT_PER_CLIENT_RPS` | Per-client requests per second | `100` |
| `RATE_LIMIT_MAX_CONCURRENT` | Max concurrent requests per client | `20` |
| **Audit Logging** | | |
| `AUDIT_ENABLED` | Enable audit logging | `false` |
| `AUDIT_LOG_FILE` | Audit log file path | - |
| `AUDIT_STDOUT` | Log audit events to stdout | `false` |
| `AUDIT_MIN_SEVERITY` | Min severity (debug/info/warning/error/critical) | `info` |
| **Tracing** | | |
| `TRACING_ENABLED` | Enable distributed tracing | `false` |
| `SERVICE_NAME` | Service name for traces | `nexus-node` |
| `OTLP_ENDPOINT` | OpenTelemetry collector endpoint | - |
| `TRACING_SAMPLE_RATE` | Trace sample rate (0.0-1.0) | `1.0` |
| `TRACING_LOG_HEADERS` | Log request headers in traces | `false` |
| **Circuit Breaker** | | |
| `CIRCUIT_BREAKER_ENABLED` | Enable circuit breakers | `false` |
| `CIRCUIT_BREAKER_FAILURE_THRESHOLD` | Failures before opening | `5` |
| `CIRCUIT_BREAKER_RECOVERY_TIMEOUT` | Recovery timeout (seconds) | `30` |
| `CIRCUIT_BREAKER_SUCCESS_THRESHOLD` | Successes to close | `3` |
| **Cloudflare** | | |
| `CLOUDFLARE_ENABLED` | Enable Cloudflare integration | `false` |
| `CLOUDFLARE_API_TOKEN` | Cloudflare API token | - |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account ID | - |
| `CLOUDFLARE_ZONE_ID` | Cloudflare zone ID | - |
| `CLOUDFLARE_DOMAIN` | Domain name | - |
| `CLOUDFLARE_SPECTRUM_ENABLED` | Enable Spectrum proxy | `false` |
| `CLOUDFLARE_DNS_AUTO_MANAGE` | Auto-manage DNS records | `false` |
| `CLOUDFLARE_TUNNEL_ENABLED` | Enable Cloudflare Tunnel | `false` |
| `CLOUDFLARE_TUNNEL_ID` | Tunnel ID | - |
| `CLOUDFLARE_PROXY_PROTOCOL_VERSION` | Proxy protocol version (1/2) | `2` |

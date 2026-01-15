# Enterprise Features

Advanced features for production deployments.

## Cloudflare Spectrum Integration

DDoS protection via Cloudflare Spectrum proxy.

### Configuration

```bash
export CLOUDFLARE_API_TOKEN=your-token
export CLOUDFLARE_ZONE_ID=your-zone-id
export SPECTRUM_ENABLED=true
```

### Features

- L4 DDoS mitigation
- TCP/UDP proxy support
- Automatic failover
- Real-time analytics

## XDP/eBPF Firewall

Kernel-level packet filtering for high-performance protection.

### Firewall Rules

```yaml
security:
  firewall_rules:
    # Rate limit connections
    - type: connection_rate
      name: rate_limit
      limit: 100/s
      action: drop

    # Limit packet size (anti-amplification)
    - type: packet_size
      name: size_limit
      max_bytes: 1500
      action: drop

    # Block specific IPs
    - type: ip_block
      name: blocked_ips
      addresses:
        - 192.168.1.100
      action: drop
```

### XDP Programs

- Connection tracking
- SYN flood protection
- UDP amplification prevention
- Per-IP rate limiting

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
TLS_CERT=/path/to/server.crt
TLS_KEY=/path/to/server.key
TLS_CA=/path/to/ca.crt
TLS_REQUIRE_CLIENT_CERT=true
```

## Rate Limiting

Per-client request rate limiting.

### Configuration

```bash
RATE_LIMIT_REQUESTS_PER_MINUTE=1000
RATE_LIMIT_BURST=100
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

### Endpoints

```bash
# Basic health
curl http://localhost:9090/health

# Detailed health
curl http://localhost:9090/health/detailed
```

### Checks Performed

- Containerd connectivity
- Disk space available
- Memory available
- gRPC server responsive
- Metrics server responsive

### Response

```json
{
  "status": "healthy",
  "checks": {
    "containerd": "ok",
    "disk": "ok",
    "memory": "ok",
    "grpc": "ok"
  },
  "version": "0.1.0",
  "uptime_seconds": 86400
}
```

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
| `GRPC_BIND` | gRPC listen address | `127.0.0.1:8080` |
| `METRICS_BIND` | Metrics listen address | `127.0.0.1:9090` |
| `CONTAINERD_SOCKET` | Containerd socket | `/run/containerd/containerd.sock` |
| `DATA_DIR` | Data directory | `/var/lib/nexus-node` |
| `TLS_CERT` | TLS certificate path | - |
| `TLS_KEY` | TLS key path | - |
| `TLS_CA` | CA certificate path | - |
| `RATE_LIMIT_REQUESTS_PER_MINUTE` | Rate limit | `1000` |
| `CLOUDFLARE_API_TOKEN` | Cloudflare API token | - |
| `SPECTRUM_ENABLED` | Enable Spectrum | `false` |
| `RUST_LOG` | Log level | `info` |

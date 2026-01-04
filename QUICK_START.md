# Quick Start Guide - Nexus Node

Get Nexus Node up and running in minutes!

## Prerequisites

- Rust 1.75+ installed
- (Optional) Containerd 1.7+ for production use

## Build

```bash
# Build the project
cargo build --release

# Or build just the node daemon
cargo build --release -p nexus-node
```

## Run (Development Mode)

The application will automatically use a mock runtime if Containerd is not available:

```bash
# Run with default settings
cargo run --release --bin nexus-node

# Or use the built binary
./target/release/nexus-node
```

## Run (Production Mode)

### 1. Start Containerd
```bash
sudo systemctl start containerd
sudo systemctl status containerd
```

### 2. Set Environment Variables
```bash
export CONTAINERD_SOCKET=/run/containerd/containerd.sock
export DATA_DIR=/var/lib/nexus-node
export GRPC_BIND=0.0.0.0:8080
export METRICS_BIND=0.0.0.0:9090
```

### 3. Create Data Directory
```bash
sudo mkdir -p /var/lib/nexus-node
sudo chown $USER:$USER /var/lib/nexus-node
```

### 4. Run the Application
```bash
cargo run --release --bin nexus-node
```

## Verify It's Working

### Check Metrics
```bash
# Metrics should be available at http://localhost:9090/metrics
curl http://localhost:9090/metrics

# Health check
curl http://localhost:9090/health
```

### Check gRPC Service
```bash
# Install grpcurl if needed
# macOS: brew install grpcurl
# Linux: See https://github.com/fullstorydev/grpcurl

# List containers (should return empty list initially)
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/ListContainers

# Get node info
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/GetNodeInfo
```

## Example: Create a Container

### 1. Create a Test Config
Save this as `test-config.yaml`:

```yaml
metadata:
  id: test-server
  name: Test Server
  game: minecraft
  version: 1.0.0
  author: test@example.com

container:
  image: "itzg/minecraft-server:latest"
  environment: {}

resources:
  cpu:
    min: 1000
    max: 2000
    shares: 1024
  memory:
    min: 1Gi
    max: 2Gi
  disk:
    min: 5Gi
    io_priority: normal

startup:
  command: "java"
  args:
    - "-jar"
    - "server.jar"
  working_dir: /home/container

networking:
  ports:
    - name: game
      internal: "25565"
      protocol: tcp
      required: true

variables: []

security:
  capabilities:
    add: []
    drop: []
  firewall_rules: []
```

### 2. Create Container via gRPC
```bash
# Using grpcurl
grpcurl -plaintext -d '{
  "config_yaml": "'"$(cat test-config.yaml | sed 's/"/\\"/g' | tr '\n' ' ')"'",
  "auto_start": false
}' localhost:8080 nexus.node.v1.NodeService/CreateContainer
```

Or use a gRPC client library in your preferred language.

## Monitor Metrics

### View Metrics in Browser
Open http://localhost:9090/metrics in your browser to see all Prometheus metrics.

### Key Metrics to Watch
- `nexus_node_containers_total` - Total containers
- `nexus_node_containers_running` - Running containers
- `nexus_node_grpc_requests_total` - gRPC request counts
- `nexus_node_grpc_request_duration_seconds` - Request latency

## Troubleshooting

### Containerd Connection Failed
If you see "Failed to connect to Containerd", the application will:
- Log a warning
- Continue running (in development mode)
- Use mock runtime for testing

To fix:
```bash
# Check Containerd is running
sudo systemctl status containerd

# Check socket permissions
ls -la /run/containerd/containerd.sock

# Add user to containerd group (if needed)
sudo usermod -aG containerd $USER
```

### Port Already in Use
```bash
# Check what's using the port
sudo lsof -i :8080
sudo lsof -i :9090

# Change the port
export GRPC_BIND=127.0.0.1:8081
export METRICS_BIND=127.0.0.1:9091
```

### Permission Denied
```bash
# Check data directory permissions
ls -la /var/lib/nexus-node

# Fix permissions
sudo chown -R $USER:$USER /var/lib/nexus-node
```

## Next Steps

1. **Set up Prometheus**: Configure Prometheus to scrape metrics from `:9090/metrics`
2. **Create Grafana Dashboard**: Visualize metrics
3. **Deploy to Production**: Use Docker or Kubernetes
4. **Add Authentication**: Secure the gRPC endpoints
5. **Scale**: Deploy multiple nodes

## Configuration Reference

All configuration is via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `GRPC_BIND` | `127.0.0.1:8080` | gRPC server bind address |
| `METRICS_BIND` | `127.0.0.1:9090` | Metrics HTTP server bind address |
| `CONTAINERD_SOCKET` | `/run/containerd/containerd.sock` | Containerd socket path |
| `CONTAINERD_NAMESPACE` | `nexus-panel` | Containerd namespace |
| `DATA_DIR` | `/var/lib/nexus-node` | Data directory for containers |
| `NODE_ID` | Hostname or `node-1` | Node identifier |
| `MIN_DISK_SPACE_BYTES` | `1073741824` (1GB) | Minimum disk space for health check |
| `MIN_MEMORY_BYTES` | `536870912` (512MB) | Minimum memory for health check |

## Logging

Control log level with `RUST_LOG`:

```bash
# Debug logging
RUST_LOG=debug cargo run --bin nexus-node

# Trace logging (very verbose)
RUST_LOG=trace cargo run --bin nexus-node

# Specific module
RUST_LOG=nexus_node=debug cargo run --bin nexus-node
```

## Support

- **Documentation**: See `INTEGRATION_COMPLETE.md` for full details
- **Enterprise Improvements**: See `ENTERPRISE_IMPROVEMENTS.md`
- **Architecture**: See `NODE_ARCHITECTURE.md`

Happy deploying! 🚀



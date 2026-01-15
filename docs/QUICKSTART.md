# Quick Start Guide

Get Nexus Panel running in minutes.

## Prerequisites

- Rust 1.75+
- Linux 5.15+ (for cgroup v2, eBPF)
- Containerd 1.7+ (for production)

## Build

```bash
# Install Rust (if needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/rifle-ak/nexus-panel.git
cd nexus-panel
cargo build --release
```

## Run the Node Daemon

### Development Mode

Uses mock runtime if Containerd is unavailable:

```bash
cargo run --release --bin nexus-node
```

### Production Mode

```bash
# Start Containerd
sudo systemctl start containerd

# Configure environment
export CONTAINERD_SOCKET=/run/containerd/containerd.sock
export DATA_DIR=/var/lib/nexus-node
export GRPC_BIND=0.0.0.0:8080
export METRICS_BIND=0.0.0.0:9090

# Create data directory
sudo mkdir -p /var/lib/nexus-node
sudo chown $USER:$USER /var/lib/nexus-node

# Run
./target/release/nexus-node
```

## Verify Installation

```bash
# Check metrics
curl http://localhost:9090/metrics
curl http://localhost:9090/health

# Check gRPC (requires grpcurl)
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/HealthCheck
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/ListContainers
```

## Convert Pterodactyl Eggs

```bash
# Single egg
./target/release/nexus-panel convert --input egg.json --output config.yaml

# Bulk import
./target/release/nexus-panel import --input-dir ./eggs --output-dir ./configs

# From GitHub
./target/release/nexus-panel clone --repo https://github.com/parkervcp/eggs

# Validate config
./target/release/nexus-panel validate --input config.yaml
```

### CLI Options

**convert** - Convert single egg:
- `--input, -i`: Path to Pterodactyl egg JSON
- `--output, -o`: Output path (optional)
- `--no-security-scan`: Skip security scanning
- `--no-firewall-rules`: Don't add default firewall rules

**import** - Bulk import:
- `--input-dir, -i`: Directory with egg JSON files
- `--output-dir, -o`: Output directory
- `--continue-on-error`: Don't stop on conversion errors

**clone** - Clone from git:
- `--repo, -r`: Git repository URL
- `--output-dir, -o`: Output directory

## Create a Container

Save as `test-config.yaml`:

```yaml
metadata:
  id: test-server
  name: Test Server
  game: minecraft
  version: 1.0.0

container:
  image: itzg/minecraft-server:latest

resources:
  cpu:
    min: 1000
    max: 2000
  memory:
    min: 1Gi
    max: 2Gi

startup:
  command: java
  args: ["-jar", "server.jar"]
  working_dir: /home/container

networking:
  ports:
    - name: game
      internal: "25565"
      protocol: tcp
```

Create via gRPC:

```bash
grpcurl -plaintext -d '{
  "config_yaml": "'"$(cat test-config.yaml)"'",
  "auto_start": false
}' localhost:8080 nexus.node.v1.NodeService/CreateContainer
```

## Configuration Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `GRPC_BIND` | `127.0.0.1:8080` | gRPC server address |
| `METRICS_BIND` | `127.0.0.1:9090` | Metrics HTTP address |
| `CONTAINERD_SOCKET` | `/run/containerd/containerd.sock` | Containerd socket |
| `CONTAINERD_NAMESPACE` | `nexus-panel` | Containerd namespace |
| `DATA_DIR` | `/var/lib/nexus-node` | Data directory |
| `NODE_ID` | Hostname | Node identifier |
| `MIN_DISK_SPACE_BYTES` | 1GB | Min disk for health check |
| `MIN_MEMORY_BYTES` | 512MB | Min memory for health check |

## Logging

```bash
# Debug logging
RUST_LOG=debug cargo run --bin nexus-node

# Specific module
RUST_LOG=nexus_node=debug cargo run --bin nexus-node
```

## Development

```bash
# Run tests
cargo test

# Format code
cargo fmt

# Lint
cargo clippy
```

## Next Steps

- See [DEPLOYMENT.md](DEPLOYMENT.md) for production setup
- See [API.md](API.md) for gRPC endpoint reference
- See [ENTERPRISE.md](ENTERPRISE.md) for advanced features

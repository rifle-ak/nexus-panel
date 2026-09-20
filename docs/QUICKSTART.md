# Quick Start Guide

Get Nexus Panel running in minutes.

## Automatic Install (Recommended)

One command installs all dependencies, builds the project, and starts the node as a systemd service:

```bash
curl -fsSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo bash -s --
```

The installer runs an **interactive setup wizard** that asks you to configure:
- Node name, domain name (optional), HTTPS via Let's Encrypt, admin password, and ports.
- Press Enter at each prompt to accept the defaults.

Or if you've already cloned the repo:

```bash
sudo bash install.sh
```

To skip the wizard and use defaults (or env vars), set `NONINTERACTIVE=1`:

```bash
curl -fsSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo NONINTERACTIVE=1 bash -s --
```

Once complete, verify:

```bash
curl http://localhost:9090/health
systemctl status nexus-node
```

## Manual Install

If you prefer to install manually, follow the steps below.

### Prerequisites

- Rust 1.85+ (latest stable recommended)
- Linux 5.15+ (cgroup v2) and `nftables` for the firewall
- Containerd 1.7+ (for production)
- Protobuf compiler (`protoc`)

### Install Dependencies

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install -y containerd protobuf-compiler gcc g++ make

# Install Rust (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Or update an existing installation
rustup update stable
```

### Build

```bash
git clone https://github.com/rifle-ak/nexus-panel.git
cd nexus-panel
cargo build --release --workspace
```

### Run the Node Daemon

#### Development Mode

Uses mock runtime if containerd is unavailable:

```bash
cargo run --release -p nexus-node --bin nexus-node
```

#### Production Mode

```bash
# Start containerd
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
nexus-panel convert --input egg.json --output config.yaml

# Bulk import
nexus-panel import --input-dir ./eggs --output-dir ./configs

# From GitHub
nexus-panel clone --repo https://github.com/parkervcp/eggs

# Validate config
nexus-panel validate --input config.yaml
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
RUST_LOG=debug cargo run -p nexus-node --bin nexus-node

# Specific module
RUST_LOG=nexus_node=debug cargo run -p nexus-node --bin nexus-node
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

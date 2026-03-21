# Production Deployment Guide

Deploy Nexus Node in a production environment.

## System Requirements

- **OS**: Linux 5.15+ (Ubuntu 22.04+, Debian 12+, RHEL 9+)
- **CPU**: 2+ cores recommended
- **Memory**: 2GB+ (plus game server requirements)
- **Disk**: 50GB+ SSD recommended
- **Network**: Static IP, open ports for game servers

## Install Dependencies

### Rust Toolchain

Rust 1.85+ (latest stable) is required to build Nexus Node. Some dependencies use Rust edition 2024 features that are not available in older toolchains.

```bash
# Install Rust (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Or update an existing installation to latest stable
rustup update stable

# Verify version (must be 1.85+)
cargo --version
```

### Containerd

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install -y containerd runc

# RHEL/CentOS
sudo dnf install -y containerd runc

# Configure
sudo mkdir -p /etc/containerd
containerd config default | sudo tee /etc/containerd/config.toml

# Enable and start
sudo systemctl enable containerd
sudo systemctl start containerd
```

### CNI Plugins

```bash
CNI_VERSION="v1.4.0"
sudo mkdir -p /opt/cni/bin
curl -L "https://github.com/containernetworking/plugins/releases/download/${CNI_VERSION}/cni-plugins-linux-amd64-${CNI_VERSION}.tgz" | sudo tar -xz -C /opt/cni/bin
```

## Install Nexus Node

```bash
# Build
cargo build --release

# Install binary
sudo cp target/release/nexus-node /usr/local/bin/
sudo chmod +x /usr/local/bin/nexus-node

# Create directories
sudo mkdir -p /var/lib/nexus-node
sudo mkdir -p /etc/nexus-node
```

## Configuration

Create `/etc/nexus-node/config.env`:

```bash
# gRPC server
GRPC_BIND=0.0.0.0:8080

# Metrics server
METRICS_BIND=0.0.0.0:9090

# Containerd
CONTAINERD_SOCKET=/run/containerd/containerd.sock
CONTAINERD_NAMESPACE=nexus-panel

# Data storage
DATA_DIR=/var/lib/nexus-node

# Node identification
NODE_ID=prod-node-1

# Health checks
MIN_DISK_SPACE_BYTES=10737418240
MIN_MEMORY_BYTES=1073741824

# TLS (optional)
TLS_ENABLED=true
TLS_CERT_PATH=/etc/nexus-node/server.crt
TLS_KEY_PATH=/etc/nexus-node/server.key
TLS_CA_CERT_PATH=/etc/nexus-node/ca.crt
TLS_REQUIRE_CLIENT_CERT=true
TLS_MIN_VERSION=1.3

# Authentication (optional)
AUTH_ENABLED=true
AUTH_API_KEYS=key1,key2

# Rate Limiting (enabled by default)
RATE_LIMIT_GLOBAL_RPS=10000
RATE_LIMIT_PER_CLIENT_RPS=100

# Audit Logging (optional)
AUDIT_ENABLED=true
AUDIT_LOG_FILE=/var/log/nexus-node/audit.log

# Logging
RUST_LOG=info
LOG_FORMAT=json
```

## Systemd Service

Create `/etc/systemd/system/nexus-node.service`:

```ini
[Unit]
Description=Nexus Node Game Server Daemon
After=network.target containerd.service
Requires=containerd.service

[Service]
Type=simple
User=root
EnvironmentFile=/etc/nexus-node/config.env
ExecStart=/usr/local/bin/nexus-node
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

# Security hardening
NoNewPrivileges=false
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/nexus-node
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable nexus-node
sudo systemctl start nexus-node
sudo systemctl status nexus-node
```

## TLS Configuration

Generate certificates for mTLS:

```bash
# Generate CA
openssl genrsa -out ca.key 4096
openssl req -new -x509 -days 3650 -key ca.key -out ca.crt \
  -subj "/CN=Nexus Panel CA"

# Generate server cert
openssl genrsa -out server.key 2048
openssl req -new -key server.key -out server.csr \
  -subj "/CN=nexus-node"
openssl x509 -req -days 365 -in server.csr -CA ca.crt -CAkey ca.key \
  -CAcreateserial -out server.crt

# Install
sudo cp ca.crt server.crt server.key /etc/nexus-node/
sudo chmod 600 /etc/nexus-node/*.key
```

## Pre-pull Game Images

```bash
# Pull images before container creation
sudo ctr -n nexus-panel images pull docker.io/itzg/minecraft-server:latest
sudo ctr -n nexus-panel images pull ghcr.io/parkervcp/steamcmd:debian
```

## Monitoring

### Prometheus

Add to `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'nexus-node'
    static_configs:
      - targets: ['localhost:9090']
```

### Key Metrics

| Metric | Description |
|--------|-------------|
| `nexus_node_containers_total` | Total containers |
| `nexus_node_containers_running` | Running containers |
| `nexus_node_grpc_requests_total` | gRPC request count |
| `nexus_node_grpc_request_duration_seconds` | Request latency |
| `nexus_node_container_cpu_usage_seconds` | CPU usage |
| `nexus_node_container_memory_bytes` | Memory usage |

## Firewall

```bash
# Allow gRPC
sudo ufw allow 8080/tcp

# Allow metrics (internal only)
sudo ufw allow from 10.0.0.0/8 to any port 9090

# Allow game server ports
sudo ufw allow 25565:25665/tcp  # Minecraft
sudo ufw allow 27015:27115/udp  # Source games
```

## Troubleshooting

### Containerd Connection

```bash
# Check socket
ls -la /run/containerd/containerd.sock

# Check service
sudo systemctl status containerd

# Test connection
sudo ctr version
```

### Port Conflicts

```bash
# Check what's using ports
sudo lsof -i :8080
sudo lsof -i :9090
```

### Permissions

```bash
# Fix data directory
sudo chown -R root:root /var/lib/nexus-node
sudo chmod 755 /var/lib/nexus-node
```

### Logs

```bash
# View logs
sudo journalctl -u nexus-node -f

# Debug logging
sudo systemctl edit nexus-node
# Add: Environment=RUST_LOG=debug
sudo systemctl restart nexus-node
```

## Security Hardening

1. **Network**: Run behind a firewall or VPN
2. **TLS**: Enable mTLS for all gRPC connections
3. **Updates**: Keep Containerd and runc updated
4. **Monitoring**: Set up alerts for unusual activity
5. **Backups**: Regular backup of `/var/lib/nexus-node`

## Performance Tuning

### System Limits

Add to `/etc/security/limits.conf`:

```
* soft nofile 65535
* hard nofile 65535
* soft nproc 65535
* hard nproc 65535
```

### Kernel Parameters

Add to `/etc/sysctl.d/99-nexus.conf`:

```
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535
vm.max_map_count = 262144
```

Apply: `sudo sysctl --system`

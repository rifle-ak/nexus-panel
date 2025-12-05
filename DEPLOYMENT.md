# Nexus Panel Deployment Guide

This guide covers deploying Nexus Node with Containerd to run real game servers.

## System Requirements

### Minimum Requirements
- **OS**: Linux 5.15+ (Ubuntu 22.04, Debian 12, or similar)
- **CPU**: 2+ cores
- **RAM**: 4GB+ (plus resources for game servers)
- **Disk**: 20GB+ (plus storage for game servers)
- **Network**: Public IP for game server hosting

### Required Software
- **Containerd** 1.7+
- **Rust** 1.75+ (for building)
- **protobuf-compiler** (for gRPC)

## Installation

### 1. Install Containerd

#### Ubuntu/Debian
```bash
# Install Containerd
sudo apt-get update
sudo apt-get install -y containerd

# Configure Containerd
sudo mkdir -p /etc/containerd
containerd config default | sudo tee /etc/containerd/config.toml

# Enable and start Containerd
sudo systemctl enable containerd
sudo systemctl start containerd

# Verify Containerd is running
sudo systemctl status containerd
```

#### Manual Installation
```bash
# Download Containerd
CONTAINERD_VERSION="1.7.11"
wget https://github.com/containerd/containerd/releases/download/v${CONTAINERD_VERSION}/containerd-${CONTAINERD_VERSION}-linux-amd64.tar.gz

# Extract to /usr/local
sudo tar Cxzvf /usr/local containerd-${CONTAINERD_VERSION}-linux-amd64.tar.gz

# Install systemd service
sudo wget -O /etc/systemd/system/containerd.service https://raw.githubusercontent.com/containerd/containerd/main/containerd.service

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable containerd
sudo systemctl start containerd
```

### 2. Install runc (OCI Runtime)

```bash
# Install runc
RUNC_VERSION="1.1.10"
wget https://github.com/opencontainers/runc/releases/download/v${RUNC_VERSION}/runc.amd64
sudo install -m 755 runc.amd64 /usr/local/sbin/runc
```

### 3. Install CNI Plugins (Networking)

```bash
# Install CNI plugins
CNI_VERSION="1.4.0"
sudo mkdir -p /opt/cni/bin
wget https://github.com/containernetworking/plugins/releases/download/v${CNI_VERSION}/cni-plugins-linux-amd64-v${CNI_VERSION}.tgz
sudo tar Cxzvf /opt/cni/bin cni-plugins-linux-amd64-v${CNI_VERSION}.tgz

# Configure CNI
sudo mkdir -p /etc/cni/net.d
cat <<EOF | sudo tee /etc/cni/net.d/10-nexus-bridge.conf
{
  "cniVersion": "1.0.0",
  "name": "nexus-bridge",
  "type": "bridge",
  "bridge": "nexus0",
  "isGateway": true,
  "ipMasq": true,
  "ipam": {
    "type": "host-local",
    "subnet": "10.88.0.0/16",
    "routes": [
      { "dst": "0.0.0.0/0" }
    ]
  }
}
EOF
```

### 4. Build Nexus Node

```bash
# Clone repository
git clone https://github.com/your-org/nexus-panel.git
cd nexus-panel

# Build in release mode
cargo build --release -p nexus-node

# Install binary
sudo cp target/release/nexus-node /usr/local/bin/

# Create data directory
sudo mkdir -p /var/lib/nexus-node
sudo chown $USER:$USER /var/lib/nexus-node
```

### 5. Configure Nexus Node

Create configuration file at `/etc/nexus-node/config.yaml`:

```yaml
# Nexus Node Configuration

daemon:
  # gRPC listen address (for panel communication)
  grpc_bind: "127.0.0.1:8080"

  # Prometheus metrics endpoint
  metrics_bind: "127.0.0.1:9090"

  # Data directory for game servers
  data_dir: "/var/lib/nexus-node"

  # Log level: trace, debug, info, warn, error
  log_level: "info"

# Containerd connection
containerd:
  socket: "/run/containerd/containerd.sock"
  namespace: "nexus-panel"

# Default resource limits
resources:
  # Default CPU limit (millicores, 1000 = 1 core)
  default_cpu_limit: 1000

  # Default memory limit (MB)
  default_memory_limit: 1024

  # Default disk limit (GB)
  default_disk_limit: 10

# Networking
network:
  # Network mode: bridge, host
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

### 6. Create Systemd Service

Create `/etc/systemd/system/nexus-node.service`:

```ini
[Unit]
Description=Nexus Node - Game Server Container Runtime
Documentation=https://github.com/your-org/nexus-panel
After=network.target containerd.service
Requires=containerd.service

[Service]
Type=simple
User=nexus
Group=nexus
ExecStart=/usr/local/bin/nexus-node --config /etc/nexus-node/config.yaml
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536

# Security
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/nexus-node

[Install]
WantedBy=multi-user.target
```

### 7. Create Service User

```bash
# Create nexus user
sudo useradd --system --no-create-home --shell /bin/false nexus

# Add to containerd group (if exists)
sudo usermod -aG containerd nexus || true

# Set permissions
sudo chown -R nexus:nexus /var/lib/nexus-node
```

### 8. Start Nexus Node

```bash
# Enable and start service
sudo systemctl daemon-reload
sudo systemctl enable nexus-node
sudo systemctl start nexus-node

# Check status
sudo systemctl status nexus-node

# View logs
sudo journalctl -u nexus-node -f
```

## Usage

### Deploy a Game Server

```bash
# Using the CLI
nexus-panel deploy --config converted_eggs/egg-paper.yaml --name my-minecraft-server

# Or via API
curl -X POST http://localhost:8080/api/servers \
  -H "Content-Type: application/json" \
  -d '{
    "config": "converted_eggs/egg-paper.yaml",
    "name": "my-minecraft-server"
  }'
```

### Manage Containers

```bash
# List running servers
nexus-panel list

# Start a server
nexus-panel start <server-id>

# Stop a server
nexus-panel stop <server-id>

# View server logs
nexus-panel logs <server-id> --follow

# Delete a server
nexus-panel delete <server-id>
```

## Monitoring

### Prometheus Metrics

Nexus Node exposes Prometheus metrics at `http://localhost:9090/metrics`:

```
# Container metrics
nexus_node_container_state{id, name}
nexus_node_container_restarts_total{id, name}
nexus_node_container_cpu_usage_seconds{id, name}
nexus_node_container_memory_bytes{id, name, type}
nexus_node_container_network_bytes{id, name, direction}

# Node metrics
nexus_node_containers_total
nexus_node_grpc_requests_total{method, status}
nexus_node_grpc_request_duration_seconds{method}
```

### Grafana Dashboard

Import the included Grafana dashboard from `grafana/nexus-node-dashboard.json`.

## Troubleshooting

### Containerd Connection Issues

```bash
# Check Containerd is running
sudo systemctl status containerd

# Test Containerd connection
sudo ctr --namespace nexus-panel containers list

# Check Containerd logs
sudo journalctl -u containerd -n 50
```

### Container Not Starting

```bash
# Check Nexus Node logs
sudo journalctl -u nexus-node -n 100

# Check container status via Containerd
sudo ctr --namespace nexus-panel containers list
sudo ctr --namespace nexus-panel tasks list

# Inspect container logs
sudo ctr --namespace nexus-panel tasks exec <container-id> cat /logs/latest.log
```

### Network Issues

```bash
# Check CNI configuration
ls -la /etc/cni/net.d/
cat /etc/cni/net.d/10-nexus-bridge.conf

# Check bridge interface
ip link show nexus0
ip addr show nexus0

# Check iptables rules
sudo iptables -t nat -L -n -v | grep nexus
```

### Permission Issues

```bash
# Ensure nexus user can access Containerd socket
sudo usermod -aG containerd nexus

# Restart Nexus Node
sudo systemctl restart nexus-node

# Check file permissions
ls -la /run/containerd/containerd.sock
ls -la /var/lib/nexus-node
```

## Security Hardening

### 1. Container Isolation

Nexus Node automatically applies these security measures:

- **Namespaces**: PID, network, mount, IPC, UTS
- **Cgroups v2**: CPU, memory, disk I/O limits
- **Seccomp**: Blocks dangerous syscalls
- **Capabilities**: Drops all except necessary ones
- **Read-only root**: Root filesystem mounted read-only where possible

### 2. Firewall Configuration

```bash
# Allow only game server ports
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp  # SSH
sudo ufw allow 25565:25665/tcp  # Game server range
sudo ufw allow 25565:25665/udp
sudo ufw enable
```

### 3. Resource Limits

Set system-wide limits in `/etc/security/limits.conf`:

```
nexus soft nofile 65536
nexus hard nofile 65536
nexus soft nproc 4096
nexus hard nproc 4096
```

### 4. Audit Logging

Enable audit logging for security events:

```bash
# Install auditd
sudo apt-get install -y auditd

# Add Nexus Node audit rules
echo "-w /var/lib/nexus-node -p wa -k nexus-node-data" | sudo tee -a /etc/audit/rules.d/nexus.rules
sudo systemctl restart auditd
```

## Performance Tuning

### 1. Kernel Parameters

Add to `/etc/sysctl.conf`:

```
# Network
net.core.somaxconn = 4096
net.ipv4.tcp_max_syn_backlog = 8192
net.ipv4.ip_local_port_range = 10000 65535

# File descriptors
fs.file-max = 1000000
fs.inotify.max_user_instances = 1024
fs.inotify.max_user_watches = 524288

# Memory
vm.max_map_count = 262144
```

Apply changes:
```bash
sudo sysctl -p
```

### 2. Containerd Optimization

Edit `/etc/containerd/config.toml`:

```toml
[plugins."io.containerd.grpc.v1.cri"]
  max_container_log_line_size = 16384

[plugins."io.containerd.grpc.v1.cri".containerd]
  default_runtime_name = "runc"

[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.runc]
  runtime_type = "io.containerd.runc.v2"

[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.runc.options]
  SystemdCgroup = true
```

Restart Containerd:
```bash
sudo systemctl restart containerd
```

## Backup and Recovery

### Backup Server Data

```bash
#!/bin/bash
# Backup script

BACKUP_DIR="/backup/nexus-node/$(date +%Y%m%d)"
mkdir -p "$BACKUP_DIR"

# Stop containers
systemctl stop nexus-node

# Backup data directory
tar czf "$BACKUP_DIR/server-data.tar.gz" /var/lib/nexus-node

# Backup configuration
cp -r /etc/nexus-node "$BACKUP_DIR/config"

# Restart
systemctl start nexus-node

echo "Backup completed: $BACKUP_DIR"
```

### Restore from Backup

```bash
#!/bin/bash
# Restore script

BACKUP_DIR="/backup/nexus-node/20241205"

# Stop Nexus Node
systemctl stop nexus-node

# Restore data
tar xzf "$BACKUP_DIR/server-data.tar.gz" -C /

# Restore configuration
cp -r "$BACKUP_DIR/config/"* /etc/nexus-node/

# Restart
systemctl start nexus-node
```

## Next Steps

1. **Deploy your first server**: Convert a Pterodactyl egg and deploy it
2. **Set up monitoring**: Configure Prometheus and Grafana
3. **Configure backups**: Set up automated backup schedule
4. **Secure your node**: Follow security hardening checklist
5. **Scale horizontally**: Add more nodes for high availability

## Support

- **Documentation**: https://docs.nexus-panel.dev
- **Issues**: https://github.com/your-org/nexus-panel/issues
- **Discord**: https://discord.gg/nexus-panel

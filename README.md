# Nexus Panel - Next-Gen Game Server Management

A high-performance game server control panel built with security and performance as first-class citizens.

## 🎯 Project Goals

Build a game server panel that takes the best of Pterodactyl/Pelican and kicks it up several notches:

- **Performance**: Rust-based Wings daemon, sub-100ms command execution
- **Security**: Built-in XDP firewall, proper secrets management, sandboxed execution
- **Features**: Multi-marketplace mod integration (Umod, Codefling, Lone.Design, etc.)
- **Developer Experience**: Native YAML configs, backward compatible with Pterodactyl eggs

## 🏗️ Architecture

```
┌─────────────────────────────────────┐
│  Panel (Rust/Go + React)            │
│  - API Gateway                      │
│  - Auth (JWT + RBAC)                │
│  - Marketplace Aggregator           │
│  - WebSocket Hub                    │
└──────────────┬──────────────────────┘
               │ gRPC/WebSocket
               ├─────────────┬─────────────┐
          ┌────▼────┐   ┌────▼────┐   ┌────▼────┐
          │ Wings 1 │   │ Wings 2 │   │ Wings N │
          │ (Rust)  │   │ (Rust)  │   │ (Rust)  │
          │         │   │         │   │         │
          │ ┌─────┐ │   │ ┌─────┐ │   │ ┌─────┐ │
          │ │ XDP │ │   │ │ XDP │ │   │ │ XDP │ │
          │ └─────┘ │   │ └─────┘ │   │ └─────┘ │
          └─────────┘   └─────────┘   └─────────┘
```

## 📦 Components

### Nexus Config Format

Our native YAML format is superior to Pterodactyl eggs:

- **Declarative**: No shell script nonsense
- **Validated**: Type-safe configuration
- **Secure**: Security policies built-in
- **Structured lifecycle hooks**: pre_start, post_start, pre_stop
- **XDP firewall rules**: Per-game DDoS protection

Example:
```yaml
metadata:
  id: rust-dedicated
  name: Rust Dedicated Server
  version: 2.0.0
  game: rust

container:
  image: ghcr.io/panel/rust-server:latest

resources:
  cpu:
    min: 2000  # millicores
    max: 4000
  memory:
    min: 4Gi
    max: 8Gi

security:
  firewall_rules:
    - type: connection_rate
      name: rate_limit
      limit: 100/s
      action: drop
```

### Egg Importer

Convert Pterodactyl eggs to native format with security enhancements.

## 🚀 Quick Start

### Installation

```bash
# Clone the repo
git clone https://github.com/rifle-ak/nexus-panel.git
cd nexus-panel

# Build
cargo build --release
```

### Import Pterodactyl Eggs

```bash
# Convert a single egg
./target/release/nexus-panel convert \
  --input eggs/rust.json \
  --output configs/rust.yaml

# Bulk import from directory
./target/release/nexus-panel import \
  --input-dir ./pterodactyl-eggs \
  --output-dir ./configs

# Clone and import from GitHub (Parker's eggs)
./target/release/nexus-panel clone \
  --repo https://github.com/parkervcp/eggs \
  --output-dir ./configs

# Validate a config
./target/release/nexus-panel validate \
  --input configs/rust.yaml
```

### CLI Options

**convert** - Convert a single egg:
- `--input, -i`: Path to Pterodactyl egg JSON
- `--output, -o`: Output path (optional, defaults to same name .yaml)
- `--no-security-scan`: Skip security scanning
- `--no-firewall-rules`: Don't add default firewall rules

**import** - Bulk import eggs:
- `--input-dir, -i`: Directory with egg JSON files
- `--output-dir, -o`: Output directory for configs
- `--no-security-scan`: Skip security scanning
- `--no-firewall-rules`: Don't add default firewall rules
- `--continue-on-error`: Don't stop on conversion errors

**clone** - Clone and import from git:
- `--repo, -r`: Git repository URL
- `--output-dir, -o`: Output directory
- `--no-security-scan`: Skip security scanning
- `--no-firewall-rules`: Don't add default firewall rules

**validate** - Validate a native config:
- `--input, -i`: Path to YAML config

## 🔍 What Gets Improved During Import

When converting Pterodactyl eggs, we automatically:

### 1. Security Scanning
- Detect dangerous commands (`rm -rf /`, `chmod 777`, `curl | bash`)
- Flag security issues in startup scripts
- Warn about privileged containers

### 2. Security Enhancements
- Drop ALL capabilities by default, only add NET_BIND_SERVICE
- Enable seccomp profiles
- Set no_new_privileges
- Add default XDP firewall rules:
  - Connection rate limiting (100/s)
  - Packet size limits (anti-amplification)

### 3. Networking
- Auto-detect ports from variables
- Set RCON ports to firewall deny by default
- Add CloudFlare DNS by default

### 4. Resource Limits
- Set sensible defaults based on game type:
  - Rust: 2-4 cores, 4-8GB RAM, 20GB disk
  - Minecraft: 1-2 cores, 2-4GB RAM, 10GB disk
  - ARK: 4-6 cores, 8-16GB RAM, 50GB disk

### 5. Monitoring
- Auto-configure health checks (RCON or TCP)
- Set up monitoring intervals and thresholds

### 6. Structured Lifecycle
- Convert installation scripts to structured hooks
- Add timeouts and conditions
- Proper stop commands

## 📊 Example Conversion

**Before (Pterodactyl Egg):**
```json
{
  "startup": "cd /home/container && ./RustDedicated -batchmode +server.port {{SERVER_PORT}}",
  "stop": "quit",
  "scripts": {
    "installation": {
      "script": "#!/bin/bash\napt-get update && apt-get install -y steamcmd"
    }
  }
}
```

**After (Native Config):**
```yaml
startup:
  command: ./RustDedicated
  args:
    - "-batchmode"
    - "+server.port"
    - "{{SERVER_PORT}}"
  working_dir: /home/container
  lifecycle:
    pre_start:
      - type: execute
        command: "apt-get update && apt-get install -y steamcmd"
        condition: first_start
        timeout: 300s
    pre_stop:
      - type: execute
        command: "quit"
        timeout: 30s

security:
  capabilities:
    drop: [ALL]
    add: [NET_BIND_SERVICE]
  firewall_rules:
    - type: connection_rate
      name: rate_limit_connections
      limit: 100/s
      action: drop
```

## 🧪 Testing

```bash
# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run -- convert --input test.json
```

## 🗺️ Roadmap

### Phase 1: Egg Importer ✅
- [x] Parse Pterodactyl eggs
- [x] Convert to native format
- [x] Security scanning
- [x] CLI tool
- [x] Bulk import from GitHub repos

### Phase 2: Wings Daemon
- [ ] Container orchestration
- [ ] XDP firewall integration
- [ ] Resource management
- [ ] Health checks
- [ ] Metrics collection

### Phase 3: Panel API
- [ ] REST/gRPC API
- [ ] Authentication & RBAC
- [ ] Server management
- [ ] File management
- [ ] WebSocket real-time updates

### Phase 4: Marketplace Integration
- [ ] Pluggable marketplace adapters
- [ ] Umod, Codefling, Lone.Design adapters
- [ ] Unified search
- [ ] Dependency resolution
- [ ] Auto-updates

### Phase 5: Frontend
- [ ] React dashboard
- [ ] Server console
- [ ] File editor
- [ ] Marketplace browser
- [ ] Monitoring dashboards

### Phase 6: WHMCS Integration
- [ ] Provisioning module
- [ ] Suspend/unsuspend
- [ ] Usage-based billing
- [ ] Customer portal

## 🤝 Contributing

This is in early development. Contributions welcome!

## 📄 License

MIT License - see LICENSE file

## 🔗 Resources

- [Pterodactyl Eggs](https://github.com/pterodactyl/panel/wiki/Egg-JSON-Format)
- [Parker's Egg Repository](https://github.com/parkervcp/eggs)
- [eBPF/XDP Documentation](https://ebpf.io/)

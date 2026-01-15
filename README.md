# Nexus Panel

A high-performance game server control panel built in Rust with security and performance as first-class citizens.

## Features

- **High Performance**: Rust-based daemon with sub-100ms command execution
- **Security First**: XDP firewall, secrets management, sandboxed execution
- **Mod Marketplace**: Unified interface for Umod, Codefling, Lone.Design
- **WHMCS Integration**: Full billing and provisioning support
- **Developer Friendly**: Native YAML configs, backward compatible with Pterodactyl eggs

## Architecture

```
┌─────────────────────────────────────┐
│  Panel (Rust + React)               │
│  - API Gateway                      │
│  - Auth (JWT + RBAC)                │
│  - Marketplace Aggregator           │
└──────────────┬──────────────────────┘
               │ gRPC/mTLS
               ├─────────────┬─────────────┐
          ┌────▼────┐   ┌────▼────┐   ┌────▼────┐
          │  Node 1 │   │  Node 2 │   │  Node N │
          │ (Rust)  │   │ (Rust)  │   │ (Rust)  │
          │ ┌─────┐ │   │ ┌─────┐ │   │ ┌─────┐ │
          │ │ XDP │ │   │ │ XDP │ │   │ │ XDP │ │
          │ └─────┘ │   │ └─────┘ │   │ └─────┘ │
          └─────────┘   └─────────┘   └─────────┘
```

## Quick Start

```bash
# Clone and build
git clone https://github.com/rifle-ak/nexus-panel.git
cd nexus-panel
cargo build --release

# Run the node daemon
./target/release/nexus-node

# Convert Pterodactyl eggs
./target/release/nexus-panel convert --input egg.json --output config.yaml
```

See [docs/QUICKSTART.md](docs/QUICKSTART.md) for detailed setup instructions.

## Native Config Format

Our YAML format replaces Pterodactyl's JSON eggs with a more secure, declarative approach:

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
    min: 2000
    max: 4000
  memory:
    min: 4Gi
    max: 8Gi

security:
  capabilities:
    drop: [ALL]
    add: [NET_BIND_SERVICE]
  firewall_rules:
    - type: connection_rate
      limit: 100/s
      action: drop
```

## Egg Import

Convert existing Pterodactyl eggs with automatic security enhancements:

```bash
# Single egg
./target/release/nexus-panel convert --input rust-egg.json

# Bulk import
./target/release/nexus-panel import --input-dir ./eggs --output-dir ./configs

# From GitHub
./target/release/nexus-panel clone --repo https://github.com/parkervcp/eggs
```

**Automatic improvements during import:**
- Security scanning for dangerous commands
- Capability dropping (principle of least privilege)
- XDP firewall rules (connection rate limiting, packet size limits)
- Resource defaults based on game type
- RCON ports set to firewall deny by default

## Project Status

All core functionality is implemented:

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | Egg Importer | Complete |
| 2 | Container Runtime | Complete |
| 3 | gRPC API | Complete |
| 4 | Marketplace | Complete |
| 5 | File/Backup/Schedule | Complete |
| 6 | WHMCS Integration | Complete |

### Implemented Features

**Container Management**
- Containerd runtime integration
- Container lifecycle (create, start, stop, restart, delete)
- Resource limits (CPU, memory, disk)
- Health checks and automatic restarts
- Bidirectional console (stdin/stdout)
- Real-time log streaming

**gRPC API**
- 12+ RPC endpoints
- mTLS authentication
- API key and JWT support
- Rate limiting and circuit breakers
- Audit logging
- Request tracing

**File Management**
- File browser (list, read, write, delete)
- Archive compression/decompression (tar.gz, zip)
- Streaming upload/download
- Directory traversal protection

**Backup System**
- Compressed backups with SHA256 verification
- Include/exclude path patterns
- Backup restore with integrity check

**Scheduled Tasks**
- Cron-based scheduling
- Console commands, power actions, backups
- Task chaining with time offsets

**Marketplace**
- Unified search across providers
- Umod, Codefling, Lone.Design adapters
- Dependency resolution
- Auto-update support

**WHMCS Integration**
- Server provisioning module
- Suspend/unsuspend hooks
- Usage-based billing
- SSO (Single Sign-On)

**Enterprise Features**
- Cloudflare Spectrum DDoS protection
- XDP/eBPF firewall
- Secrets management
- Configuration hot reload
- Graceful degradation

## Documentation

| Document | Description |
|----------|-------------|
| [QUICKSTART](docs/QUICKSTART.md) | Getting started guide |
| [DEPLOYMENT](docs/DEPLOYMENT.md) | Production deployment |
| [API](docs/API.md) | gRPC API reference |
| [ARCHITECTURE](docs/ARCHITECTURE.md) | Technical architecture |
| [ENTERPRISE](docs/ENTERPRISE.md) | Enterprise features |
| [TROUBLESHOOTING](docs/TROUBLESHOOTING.md) | Error handling |

## Project Structure

```
nexus-panel/
├── crates/
│   ├── nexus-node/        # Node daemon (container runtime, gRPC server)
│   ├── nexus-config/      # Native YAML config format
│   ├── nexus-marketplace/ # Mod marketplace integration
│   ├── nexus-whmcs/       # WHMCS billing integration
│   └── egg-importer/      # Pterodactyl egg converter
├── src/                   # CLI tool
├── docs/                  # Documentation
└── examples/              # Example configs
```

## Contributing

Contributions welcome! Please read the documentation before submitting PRs.

## License

MIT License - see LICENSE file

# Nexus Panel

A high-performance game server control panel built in Rust with security and performance as first-class citizens.

## Features

- **High Performance**: Rust-based daemon with sub-100ms command execution
- **Security First**: XDP firewall, secrets management, sandboxed execution
- **Blueprints**: Superior game server configs with performance tuning, auto-scaling, mod support
- **Mod Marketplace**: Unified interface for Umod, Codefling, Lone.Design
- **WHMCS Integration**: Full billing and provisioning support

## Blueprints vs Eggs

Nexus uses **Blueprints** - a superior alternative to Pterodactyl eggs with features eggs don't have:

| Feature | Pterodactyl Eggs | Nexus Blueprints |
|---------|------------------|------------------|
| Format | JSON | YAML |
| Performance Tuning | No | JVM flags, kernel params, CPU affinity |
| Auto-Scaling | No | Scale resources based on player count |
| Mod Support | Basic | Built-in marketplace integration |
| Health Checks | Basic | TCP, HTTP, RCON, Game Query protocols |
| Metrics | No | Player count, TPS, custom metrics |
| Update Detection | No | SteamCMD, HTTP, Docker tracking |
| Backup Config | No | Paths, exclusions, retention, scheduling |
| Clustering | No | Multi-instance load balancing |
| Dependencies | No | Database, Redis auto-provisioning |

### Official Blueprints

Ready-to-use blueprints for popular games in `/blueprints`:

- **minecraft-paper.yaml** - Paper server with Aikar's flags, auto-scaling
- **rust.yaml** - Rust with Oxide support, DDoS protection
- **valheim.yaml** - Valheim with BepInEx mod support
- **cs2.yaml** - Counter-Strike 2 with GSLT, competitive configs
- **palworld.yaml** - Palworld with optimized settings

## Quick Start

```bash
# Clone and build
git clone https://github.com/rifle-ak/nexus-panel.git
cd nexus-panel
cargo build --release

# Run the node daemon
./target/release/nexus-node

# Convert Pterodactyl eggs to blueprints
./target/release/nexus-panel convert --input egg.json --output blueprint.yaml
```

See [docs/QUICKSTART.md](docs/QUICKSTART.md) for detailed setup instructions.

## Blueprint Format

```yaml
blueprint_version: "1.0"

metadata:
  id: minecraft-paper
  name: Minecraft Paper Server
  version: "1.21.0"
  game: minecraft

container:
  image: ghcr.io/parkervcp/yolks:java_21

resources:
  cpu:
    min: 2000
    max: 4000
  memory:
    min: 4Gi
    max: 8Gi

# Nexus-exclusive: Performance tuning
performance:
  jvm:
    gc: g1gc
    aikar_flags: true
    initial_heap: "4G"
    max_heap: "4G"
  nice: -5

# Nexus-exclusive: Auto-scaling
scaling:
  enabled: true
  metric: player_count
  scale_up_threshold: 15
  scale_down_threshold: 5

# Nexus-exclusive: Mod support
mods:
  loader: bukkit
  mods_dir: /plugins
  marketplaces: [spigot, modrinth]
  auto_update: false

# Nexus-exclusive: Update detection
updates:
  check:
    type: http
    url: "https://api.papermc.io/v2/projects/paper/versions/{{MC_VERSION}}"
  auto_update: false

security:
  capabilities:
    drop: [ALL]
    add: [NET_BIND_SERVICE]
  firewall_rules:
    - type: connection_rate
      limit: 100/s
      action: drop

monitoring:
  health_check:
    type: game_query
    protocol: minecraft
    port: "{{SERVER_PORT}}"
  metrics:
    - type: game_query
      name: players_online
      protocol: minecraft
      field: players

backups:
  paths: [/world, /plugins]
  exclude: ["*.log"]
  retention: 7
  pre_backup_command: "save-all"
```

## Egg Import

Convert existing Pterodactyl eggs with automatic enhancements:

```bash
# Single egg
./target/release/nexus-panel convert --input rust-egg.json

# Bulk import
./target/release/nexus-panel import --input-dir ./eggs --output-dir ./blueprints
```

**Automatic improvements during import:**
- Performance tuning based on game type (JVM flags for Java games)
- Mod support detection (Oxide, Bukkit, BepInEx)
- Backup configuration with smart defaults
- Update detection from SteamCMD scripts
- Security hardening (capability dropping, firewall rules)

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
├── blueprints/           # Official game server blueprints
├── crates/
│   ├── nexus-node/       # Node daemon (container runtime, gRPC server)
│   ├── nexus-config/     # Blueprint format definition
│   ├── nexus-marketplace/# Mod marketplace integration
│   ├── nexus-whmcs/      # WHMCS billing integration
│   └── egg-importer/     # Pterodactyl egg converter
├── src/                  # CLI tool
└── docs/                 # Documentation
```

## Contributing

Contributions welcome! Please read the documentation before submitting PRs.

## License

MIT License - see LICENSE file

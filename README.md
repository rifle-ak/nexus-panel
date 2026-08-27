# Nexus Panel

A high-performance game server control panel built in Rust with security and performance as first-class citizens.

## Features

- **Built-in Web Panel**: Dark-themed admin UI — no separate frontend, no PHP, no MySQL
- **Single Binary**: Panel + node + API all compiled into one binary via `install.sh`
- **High Performance**: Rust-based daemon
- **Authenticated by default**: The panel requires a login (password or API key) and binds to
  loopback unless you deliberately expose it — see [SECURITY.md](SECURITY.md)
- **Blueprints**: Game server configs with performance tuning and mod support
- **Mod Marketplace**: Unified search across Umod, Codefling, Lone.Design, and the Steam Workshop
  (the only mod source for DayZ, Arma and friends) — see [Steam Workshop mods](#steam-workshop-mods)
- **WHMCS Integration**: Billing/provisioning integration (backend implemented; see the roadmap)

> **Status:** Nexus is under active development. Some features listed below are implemented and
> wired into the UI; others are backend-only or planned. See **[ROADMAP.md](ROADMAP.md)** for the
> authoritative, per-feature status, and **[docs/PRODUCTION_READINESS.md](docs/PRODUCTION_READINESS.md)**
> for what remains before a production deployment.

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
| Update Detection | No | SteamCMD, DepotDownloader, HTTP, Docker tracking |
| Backup Config | No | Paths, exclusions, retention, scheduling |
| Clustering | No | Multi-instance load balancing |
| Dependencies | No | Database, Redis auto-provisioning |
| Game Install | Install script + container | Same, plus derived from lifecycle/update config, with SteamCMD bootstrapped for you |

### Official Blueprints

Ready-to-use blueprints for popular games in `/blueprints`:

- **minecraft-paper.yaml** - Paper server with Aikar's flags, auto-scaling
- **rust.yaml** - Rust with Oxide support, DDoS protection
- **rust-carbon.yaml** - Rust with the Carbon framework, installed via DepotDownloader
- **valheim.yaml** - Valheim with BepInEx mod support
- **cs2.yaml** - Counter-Strike 2 with GSLT, competitive configs
- **palworld.yaml** - Palworld with optimized settings
- **dayz.yaml** - DayZ with Steam Workshop mod support

## Steam Workshop mods

Some games have no third-party plugin site at all — DayZ, Arma 3, Project Zomboid
and Space Engineers distribute every mod through the Steam Workshop. Nexus treats
the Workshop as a marketplace provider (`steam_workshop`) alongside Umod and the
Rust plugin sites, so the same search → detail → **Install to server** flow works
for them.

Two things make the Workshop different from an HTTP marketplace, and both are
handled for you:

- **Downloads run through SteamCMD or DepotDownloader.** Workshop files have no
  public download URL, so Nexus shells out to `steamcmd +workshop_download_item`
  — or to [DepotDownloader](https://github.com/SteamRE/DepotDownloader)
  (`-pubfile`), which is the more reliable of the two when Steam's content
  servers misbehave. `STEAM_WORKSHOP_DOWNLOADER=auto` tries SteamCMD and falls
  back to DepotDownloader automatically.
- **An item is a folder, not a file.** Nexus installs it as `@ModName` for the
  DayZ/Arma engines and as the Workshop id elsewhere, replacing any previous
  install. For DayZ, add those folder names to the blueprint's `MODS` variable
  (`-mod=@CF;@Trader`).
- **Signature keys are handled.** DayZ and Arma reject clients when a mod's
  `.bikey` is missing from the server's `keys/` directory, so Nexus copies each
  mod's keys there as part of installing it.

Installs run as a background job and the panel polls for progress — a multi-
gigabyte mod would otherwise hold an HTTP request open for the whole download.

Configuration (all optional):

| Variable | Purpose |
|----------|---------|
| `STEAM_API_KEY` | Enables Workshop *search*. Without it you can still install by pasting an item id or `steamcommunity.com` URL into the search box. Get one at [steamcommunity.com/dev/apikey](https://steamcommunity.com/dev/apikey). |
| `STEAM_USERNAME` | Steam account for downloads. **Required for DayZ and Arma**, whose Workshops refuse anonymous SteamCMD logins. |
| `STEAM_PASSWORD` | Optional. Prefer running `steamcmd +login <user>` once on the node so the credential (and Steam Guard) is cached and the secret never reaches a command line. |
| `STEAMCMD_PATH` | Path to the `steamcmd` binary (default: `steamcmd` on `PATH`). |
| `DEPOTDOWNLOADER_PATH` | Path to the `DepotDownloader` binary (default: `DepotDownloader` on `PATH`). |
| `STEAM_WORKSHOP_DOWNLOADER` | `steamcmd` (default), `depot_downloader`, or `auto` to try SteamCMD then fall back to DepotDownloader. |
| `STEAM_WORKSHOP_CACHE_DIR` | Where Workshop content is downloaded. Kept between runs so re-downloads are incremental. |
| `STEAM_WORKSHOP_TIMEOUT_SECS` | Per-download timeout (default 1800). |

## Quick Start

One command installs everything (Rust, containerd, protobuf, CNI plugins) and starts the node:

```bash
curl -fsSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo bash
```

> **Note:** while this repository is private, the URL above returns `404: Not Found`
> for everyone, including collaborators — `raw.githubusercontent.com` does not accept
> your normal GitHub login. Until the repo is made public, use one of the
> authenticated methods below.

<details>
<summary>Installing while the repository is private</summary>

Using the [GitHub CLI](https://cli.github.com/) (reads your existing `gh auth login` credentials):

```bash
gh api repos/rifle-ak/nexus-panel/contents/install.sh \
  -H 'Accept: application/vnd.github.raw' | sudo bash
```

Or with a personal access token that has `repo` scope:

```bash
curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" \
  https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo bash
```

Or just clone and run it locally (see below).

</details>

Or if you've already cloned the repo:

```bash
sudo bash install.sh
```

Once running, verify with:

```bash
curl http://localhost:9090/health
```

See [docs/QUICKSTART.md](docs/QUICKSTART.md) for manual setup or [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) for production configuration.

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

# How the game's own files get there. A container image supplies the tooling —
# a JVM, SteamCMD's libraries — not the game, so this runs once, in its own
# container, with the server's directory mounted into it, before the server is
# ever started. Optional: omit it and the install is derived from
# `startup.lifecycle.pre_start`, then from `updates.apply`.
install:
  # Defaults to container.image; imported eggs name their own installer image.
  image: ghcr.io/parkervcp/installers:debian
  entrypoint: bash
  server_dir: /home/container
  timeout: 3600s
  script: |
    curl -fsSL -o paper.jar \
      "https://api.papermc.io/v2/projects/paper/versions/{{MC_VERSION}}/builds/{{PAPER_BUILD}}/downloads/paper-{{MC_VERSION}}-{{PAPER_BUILD}}.jar"

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

You can also import from the panel itself: **Blueprints → Import a Pterodactyl
egg**. Paste an exported egg and Nexus shows the converted blueprint, what it
detected (game, image, variables, ports), and any risky commands found in the
egg's scripts — then lets you create a server from it. Nothing is deployed
until you review the result.

**Automatic improvements during import:**
- Game detection from the startup binary as well as the egg's name/image
- Performance tuning based on game type (JVM flags for Java games)
- Mod support detection (Oxide, Bukkit, BepInEx)
- Backup configuration with smart defaults
- Update detection from SteamCMD scripts
- Security hardening (capability dropping, firewall rules)
- A security report flagging dangerous patterns (`curl | bash`, `rm -rf /`,
  `chmod 777`, …) in the egg's startup and install scripts

## Architecture

```
┌─────────────────────────────────────┐
│  Panel (Rust + embedded web UI)     │
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

Core subsystems are implemented and covered by tests:

| Area | Description | Status |
|-------|-------------|--------|
| 1 | Egg Importer | Implemented |
| 2 | Container Runtime (containerd) | Implemented |
| 3 | gRPC API | Implemented |
| 4 | Marketplace | Implemented |
| 5 | File / Backup / Schedule | Implemented |
| 6 | WHMCS Integration | Backend implemented |

For a feature-by-feature breakdown (Done / Backend Ready / Planned) see
[ROADMAP.md](ROADMAP.md); for the path to a production deployment see
[docs/PRODUCTION_READINESS.md](docs/PRODUCTION_READINESS.md).

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

# Nexus Panel Roadmap

Current status of all features. Items marked **Done** are functional in the web UI. Items marked **Backend Ready** have backend implementations but no UI yet. Items marked **Planned** are documented but not yet implemented.

## Server Management

| Feature | Status |
|---------|--------|
| Container create / start / stop / restart / delete | Done |
| Container suspend / unsuspend | Done |
| Console (xterm.js) — game-process stdin | Done |
| **Real shell console (exec)** — run arbitrary commands (e.g. `npm`, shell) in the container, separate from the game-process/RCON console | Done (UI + API; containerd exec pending real-runtime validation) |
| File manager (browse, edit, create, delete, rename) | Done |
| Backups (create, restore, delete) | Done |
| Cron-based scheduling | Done |
| State persistence + reconciliation across restarts | Done |

> **Planned differentiator — real shell console.** Pterodactyl and Pelican pipe
> console input to the game process's stdin (often just an RCON bridge), so you
> can't run arbitrary commands like `npm install` in the container. containerd
> supports exec'ing a new process inside a running task, so Nexus can offer a
> genuine shell console alongside the game console — a real gap in existing
> panels.

## Blueprints

| Feature | Status |
|---------|--------|
| Per-game YAML configs (Minecraft, Rust, Valheim, CS2, Palworld) | Done |
| Blueprint selection auto-populates create form | Done |
| Custom YAML editor | Done |
| Blueprint import from Pterodactyl eggs | Planned |

## Mod Marketplace

| Feature | Status |
|---------|--------|
| Unified search across providers (Umod, Codefling, Lone.Design) | Backend (adapters need updating for current provider APIs) |
| Mod detail view | Done (UI) |
| One-click mod install to running server | Done (API + UI) |
| Mod update checking for installed mods | Planned |
| Dependency resolution | Backend Ready |

> **Note:** the marketplace adapters were written against earlier provider APIs;
> some responses have since drifted (e.g. Umod now returns `title`/`category_tags`
> rather than `name`/`category`), so live search/detail may fail to parse until the
> adapters are refreshed. The install pipeline (download → checksum → place in the
> server's mods directory) and its API/UI are complete and work against any adapter
> that parses correctly.

## Monitoring & Analytics

| Feature | Status |
|---------|--------|
| Dashboard stats (memory, disk, uptime, server count) | Done |
| Node health checks | Done |
| Prometheus metrics export | Done |
| Per-container resource metrics (CPU, memory, network, disk I/O) | Backend Ready |
| gRPC request latency tracking | Backend Ready |

## Security

| Feature | Status |
|---------|--------|
| API key authentication | Backend Ready |
| JWT authentication | Backend Ready |
| Rate limiting (global + per-client) | Backend Ready |
| TLS / mTLS for gRPC | Backend Ready |
| Audit logging (JSON format) | Backend Ready |
| Input validation (container ID, file paths, YAML) | Backend Ready |
| XDP/eBPF firewall UI (SYN flood, UDP amplification, rate limiting) | Planned |
| Per-server firewall rule configuration | Planned |
| Cloudflare Spectrum integration | Planned |

## User Management

| Feature | Status |
|---------|--------|
| Sub-user permissions (per-server) | Planned |
| API key management UI | Planned |
| Audit trail viewer in UI | Planned |
| Role-based access control | Planned |

## Infrastructure

| Feature | Status |
|---------|--------|
| Single-binary deployment | Done |
| Automated install script | Done |
| HTTPS via Let's Encrypt + Caddy | Done |
| Systemd service integration | Done |
| Software update check from UI | Done |
| Configuration hot reload | Backend Ready |
| Circuit breakers | Backend Ready |
| Distributed tracing (OpenTelemetry) | Backend Ready |
| WHMCS billing integration | Planned |

## Blueprints - Advanced Features

| Feature | Status |
|---------|--------|
| Auto-scaling (player count / CPU / memory) | Planned |
| JVM performance tuning (Aikar's flags) | Done (in YAML) |
| Kernel parameter tuning | Planned |
| Update detection (SteamCMD, HTTP, Docker) | Planned |
| Database / Redis auto-provisioning | Planned |
| Multi-instance clustering | Planned |

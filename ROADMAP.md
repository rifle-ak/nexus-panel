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
| On-demand game-file update (SteamCMD / DepotDownloader) | Done (UI + API; runs as a background job) |
| Blueprint persistence per server (survives node restart) | Done |
| One-click update using the server's own blueprint strategy | Done (no retyping; manual override still available) |
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
| Per-game YAML configs (Minecraft, Rust, Valheim, CS2, Palworld, DayZ) | Done |
| Rust (Carbon framework, DepotDownloader install) blueprint | Done (`blueprints/rust-carbon.yaml`) |
| Blueprint selection auto-populates create form | Done |
| Custom YAML editor | Done |
| Blueprint import from Pterodactyl eggs | Done (CLI **and** panel UI, with a security report) |

## Mod Marketplace

| Feature | Status |
|---------|--------|
| Umod search / detail / install | **Done** (live-verified against the current umod.org API) |
| Codefling search / detail / install | Backend (requires a Codefling API key; unauthenticated requests 401) |
| Lone.Design search / detail / install | Backend (blocked by Cloudflare bot protection for non-browser clients) |
| Steam Workshop detail / install (by item id or URL) | **Done** (needs SteamCMD on the node; `STEAM_USERNAME` for DayZ/Arma) |
| Steam Workshop text search | Done, requires `STEAM_API_KEY` + a game filter |
| Mod detail view | Done (UI) |
| One-click mod install to running server | Done (API + UI) |
| Framework selection for Rust mods (Oxide **or** Carbon) | Done (installs to `oxide/plugins` or `carbon/plugins`) |
| Mod update checking for installed mods | Planned |
| Dependency resolution | Backend Ready |

> **Provider status.** The **Umod** adapter has been refreshed for the current
> umod.org API (which now returns `title`/`downloads`/`category_tags`/
> `games_detail` and RFC3339 `*_atom` timestamps) and is verified end-to-end:
> search → detail → download (SHA-1 verified, class-named `.cs` file). **Codefling**
> uses the Invision Community REST API and rejects unauthenticated requests with
> `401 NO_API_KEY`, so it needs a configured API key; the adapter now surfaces that
> as a clear "authentication required" error. **Lone.Design** sits behind
> Cloudflare's bot challenge and returns a `403` interstitial to plain HTTP clients,
> so it isn't reachable without a browser/JS-challenge path; the adapter now reports
> that explicitly. **Steam Workshop** is the only mod source for DayZ, Arma 3,
> Project Zomboid and Space Engineers; it uses the Steam Web API for metadata and
> SteamCMD for downloads (Workshop content has no public HTTP URL), and installs
> an item as a mod *folder* — `@ModName` for the DayZ/Arma engines, the Workshop
> id elsewhere. Search needs a `STEAM_API_KEY` and a game filter (Steam has no
> cross-app Workshop search), but installing by pasted item id/URL needs neither.
> Games whose Workshop refuses anonymous downloads need `STEAM_USERNAME` on the
> node. The install pipeline (download → checksum → place in the server's
> mods directory) and its API/UI are complete and work against any adapter that
> parses correctly.

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
| Customer sessions scoped to their own server (via billing-portal SSO) | Done (enforced by the node) |
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
| WHMCS billing integration (provisioning module: create / suspend / unsuspend / terminate / change package / SSO / usage) | Done (`whmcs/`, see `docs/WHMCS.md`) |
| Provisioning REST API for billing systems (`/api/v1/provision`, idempotent, port allocation) | Done |

## Blueprints - Advanced Features

| Feature | Status |
|---------|--------|
| Auto-scaling (player count / CPU / memory) | Planned |
| JVM performance tuning (Aikar's flags) | Done (in YAML) |
| Kernel parameter tuning | Planned |
| Update detection (SteamCMD, DepotDownloader, HTTP, Docker) | Backend Ready (config schema; blueprint-declared) |
| Per-server update executor (run SteamCMD/DepotDownloader on demand) | Done (API + UI; background job) |
| Database / Redis auto-provisioning | Planned |
| Multi-instance clustering | Planned |

# Production Deployment Guide

Deploy Nexus Node in a production environment.

## Automatic Install

The fastest way to get a production node running:

```bash
curl -fsSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo bash -s --
```

The installer launches an **interactive setup wizard** that walks you through:

1. **Node name** — identify this server in multi-node setups
2. **Domain name** — use `panel.example.com` instead of a raw IP (optional)
3. **Automatic HTTPS** — free TLS via Let's Encrypt + Caddy reverse proxy (if using a domain)
4. **Authentication** — set an admin password (auto-generates one if you skip)
5. **Port configuration** — customize gRPC and metrics ports

Just press Enter to accept sensible defaults at each step.

### Non-interactive mode

Skip the wizard entirely by setting `NONINTERACTIVE=1` and passing config via environment variables:

```bash
curl -fsSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | \
  sudo NONINTERACTIVE=1 \
  PANEL_DOMAIN=panel.example.com \
  ENABLE_TLS=true \
  LETSENCRYPT_EMAIL=admin@example.com \
  ENABLE_AUTH=true \
  AUTH_PASSWORD=my-secure-password \
  NODE_ID=prod-node-1 \
  bash -s --
```

### Using a domain name

If you own a domain, point an **A record** to your server's IP before running the installer:

| Type | Name | Value |
|------|------|-------|
| A | panel.example.com | 203.0.113.42 |

When the wizard asks for a domain, enter it and choose "yes" for Let's Encrypt. The installer will:
- Install [Caddy](https://caddyserver.com) as a reverse proxy
- Automatically obtain and renew TLS certificates
- Serve your panel at `https://panel.example.com`
- Open ports 80 (ACME challenges) and 443 (HTTPS) in the firewall

No manual certificate management required — it just works.

If you prefer manual control, follow the steps below.

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

### System Packages

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install -y containerd runc protobuf-compiler gcc g++ make

# RHEL/CentOS
sudo dnf install -y containerd runc protobuf-compiler gcc gcc-c++ make
```

### Containerd

```bash
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

> **Container networking.** Game servers currently run in the *host's* network
> namespace: a server binds its allocated port directly on the node, and there
> is no port mapping to configure. Plan allocations accordingly — two servers
> cannot share a port — and keep the firewall rules below in mind, since a
> container's listening sockets are the host's.

## Install Nexus Node

```bash
# Build
cargo build --release --workspace

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

# Snapshotter used for container root filesystems. Defaults to overlayfs,
# which is what containerd unpacks images into out of the box; change it only
# if the node is configured with a different default snapshotter.
# NEXUS_SNAPSHOTTER=overlayfs

# Data storage
DATA_DIR=/var/lib/nexus-node

# The unprivileged user game servers run as (never root). Their files under
# DATA_DIR belong to it; the installer creates the account. 988 is the id
# Pterodactyl uses, so a migrated node keeps its file ownership.
NEXUS_CONTAINER_UID=988
NEXUS_CONTAINER_GID=988

# Console logs are trimmed past this many bytes, keeping the last 4 MiB.
# NEXUS_CONSOLE_LOG_MAX_BYTES=33554432

# DDoS protection (nftables). auto: on when nft is installed and the node
# runs as root; on: a missing nft is a startup error; off.
NEXUS_FIREWALL=auto
# Per-source limits on game ports: new TCP connections/s, UDP packets/s.
# NEXUS_FIREWALL_SYN_PER_SOURCE=50
# NEXUS_FIREWALL_UDP_PER_SOURCE=2000
# Global new-connection ceiling across all game ports.
# NEXUS_FIREWALL_SYN_GLOBAL=20000
# Addresses never filtered (comma-separated CIDRs): your own, monitoring.
# NEXUS_FIREWALL_TRUSTED=

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
# Keys from the environment; keys minted on the Users page live in
# DATA_DIR/.nexus/users.json alongside panel accounts.
AUTH_API_KEYS=key1,key2

# Billing / provisioning (optional — see docs/WHMCS.md)
# The address customers connect to; the node cannot discover it reliably.
NODE_PUBLIC_IP=203.0.113.10
# Ports handed to provisioned servers (inclusive; default 20000-29999).
PROVISION_PORT_RANGE=20000-29999

# Rate Limiting (enabled by default)
RATE_LIMIT_GLOBAL_RPS=10000
RATE_LIMIT_PER_CLIENT_RPS=100

# Audit Logging (optional)
AUDIT_ENABLED=true
AUDIT_LOG_FILE=/var/log/nexus-node/audit.log

# Steam Workshop mods (optional — see below)
STEAM_API_KEY=
STEAM_USERNAME=
STEAMCMD_PATH=/usr/games/steamcmd
DEPOTDOWNLOADER_PATH=/usr/local/bin/DepotDownloader
STEAM_WORKSHOP_DOWNLOADER=auto
STEAM_WORKSHOP_CACHE_DIR=/var/lib/nexus-node/workshop

# Logging
RUST_LOG=info
LOG_FORMAT=json
```

### Steam Workshop mods

The Workshop is the only mod source for DayZ, Arma 3, Project Zomboid and
Space Engineers. Nexus downloads Workshop items with SteamCMD or
DepotDownloader, so the node needs at least one of those binaries — set
`STEAMCMD_PATH` / `DEPOTDOWNLOADER_PATH` if they aren't on `PATH`.

| Variable | Effect |
|----------|--------|
| `STEAM_API_KEY` | Enables Workshop **search**. Without it, operators can still install by pasting an item id or `steamcommunity.com` URL. Create one at [steamcommunity.com/dev/apikey](https://steamcommunity.com/dev/apikey). |
| `STEAM_USERNAME` | Steam account used for downloads. **Required for DayZ and Arma** — their Workshops reject anonymous logins with `No subscription`. |
| `STEAM_PASSWORD` | Optional, and best left unset. |
| `STEAM_WORKSHOP_DOWNLOADER` | `steamcmd` (default), `depot_downloader`, or `auto`. |
| `STEAMCMD_PATH` / `DEPOTDOWNLOADER_PATH` | Binary locations. |
| `STEAM_WORKSHOP_CACHE_DIR` | Download cache, kept between runs so re-downloads are incremental. Size it for the mods you host — Arma/DayZ mod sets reach tens of gigabytes. |
| `STEAM_WORKSHOP_TIMEOUT_SECS` | Per-download timeout (default `1800`). |

#### Choosing a downloader

SteamCMD is the default because it's already on most game nodes. It is also
the one that fails badly when Steam's content servers are having a bad day —
opaque `Failure` results, or a download that never progresses.
[DepotDownloader](https://github.com/SteamRE/DepotDownloader) fetches the same
content over a different path and tends to succeed where SteamCMD doesn't,
which is why many operators keep it around (Nexus already offers it as a
game-file update strategy).

Set `STEAM_WORKSHOP_DOWNLOADER=auto` to try SteamCMD first and fall back to
DepotDownloader on failure; the node logs which tool succeeded, and a failure
that exhausts both reports what each one said. Use `depot_downloader` to skip
SteamCMD entirely.

Prefer **cached credentials** over `STEAM_PASSWORD`: run

```bash
sudo -u nexus steamcmd +login <steam-user> +quit
```

once on the node and answer the Steam Guard prompt. SteamCMD stores the
credential for that user, `+login <user>` then succeeds unattended, and the
password never appears in a command line (where any local user could read it
from `ps`). Use a dedicated Steam account that owns the game rather than a
personal one — a Workshop download counts as a login from this host.

Nexus installs each item as a mod folder inside the server directory: `@ModName`
for the DayZ/Arma engines, the Workshop id for everything else. For DayZ, list
those folder names in the server's `MODS` variable so they reach `-mod=`.

A mod's `.bikey` signature keys are copied into the server's `keys/` directory
as part of the install. DayZ and Arma verify mod signatures by default
(`verifySignatures=2`), and a missing key presents as clients being unable to
join rather than as a key error — so this is done for you, and the panel
reports how many keys were installed.

Installs run as a background job: `POST /api/v1/containers/:id/mods/install`
returns immediately with a job, and `GET` on the same path reports progress.
A multi-gigabyte Workshop download would otherwise hold an HTTP request open
for its entire duration.

## Billing integration

The WHMCS provisioning module and the node settings it needs
(`AUTH_API_KEYS`, `NODE_PUBLIC_IP`, `PROVISION_PORT_RANGE`) are covered in
[WHMCS.md](WHMCS.md). One WHMCS server record per node; a server group spreads
orders across nodes.

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

## Updating Nexus Node

### From the panel (recommended)

**Settings → Software Updates** shows what this node is running, what its
channel has available, and applies it in one click.

The update rebuilds from source and restarts the node, so the panel is
unavailable for a few minutes. **Running game servers are not affected** —
their containerd shims are independent of the node process, and the node
reconciles their state when it comes back.

If the new build fails to start, the previous binary is restored automatically
and the panel reports the rollback. The node is left running either way.

The update is refused while a server is installing its game files, since
restarting mid-install would leave a half-downloaded game.

> In-panel updates need systemd, which the installer sets up. On a node without
> it, use the command-line update below.

### Update channels

```bash
# /etc/nexus-node/config.env
UPDATE_CHANNEL=stable   # published release tags (default)
UPDATE_CHANNEL=main     # the main branch, which the installer builds by default
```

On `stable` the panel compares this node's version against the latest release.
On `main` it compares the commit the binary was built from against the branch
tip, and reports how many commits behind it is — the crate version does not
move between releases, so it cannot answer that question on its own.

A node that cannot reach GitHub reports the check as unknown, with the reason.
It does not claim to be up to date.

### Quick Update (from the command line)

Re-run the installer — it detects an existing installation and only rebuilds/restarts:

```bash
curl -fsSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo bash -s -- --update
```

Or from a local clone:

```bash
git pull origin main
sudo bash install.sh --update
```

The `--update` flag skips the setup wizard and only:
1. Updates system packages
2. Updates the Rust toolchain
3. Stops the running service (required — Linux prevents overwriting a running binary)
4. Rebuilds and installs the new binary
5. Reinstalls the systemd unit, so unit changes reach existing nodes
6. Adds any configuration keys this version introduced
7. Starts the `nexus-node` service

Your configuration in `/etc/nexus-node/config.env` is preserved: an existing
value is never rewritten, and a key you commented out is not resurrected. Only
genuinely new keys are appended, with a comment saying what they do.

> Because step 5 rewrites `/etc/systemd/system/nexus-node.service`, customise
> the unit with `sudo systemctl edit nexus-node` — a drop-in override survives
> updates, direct edits to the unit file do not.

### Manual Update

```bash
# 1. Pull latest source
cd /path/to/nexus-panel
git pull origin main

# 2. Rebuild
cargo build --release --workspace

# 3. Stop the service (must stop before replacing — Linux won't overwrite a running binary)
sudo systemctl stop nexus-node

# 4. Backup and replace the binary
sudo cp /usr/local/bin/nexus-node /usr/local/bin/nexus-node.bak
sudo cp target/release/nexus-node /usr/local/bin/nexus-node

# 5. Start
sudo systemctl start nexus-node
sudo systemctl status nexus-node
```

### Update System Dependencies

Keep containerd, runc, and OS packages up to date separately:

```bash
# Ubuntu/Debian
sudo apt-get update && sudo apt-get upgrade -y containerd runc

# RHEL/CentOS
sudo dnf upgrade -y containerd runc

# Update Rust toolchain
rustup update stable
```

### Rollback

If an update causes issues, restore the previous binary:

```bash
# The installer backs up the previous binary before replacing it
sudo cp /usr/local/bin/nexus-node.bak /usr/local/bin/nexus-node
sudo systemctl restart nexus-node
```

## Installing a Game's Files

Creating a server installs its game files automatically: the blueprint's
install script runs in its own short-lived container with the server's
directory mounted into it, and the server stays un-startable until it
succeeds. Progress is in the panel's **Install** tab, or over the API:

```bash
# Re-run an install (repair, or after fixing a Steam credential)
curl -X POST http://localhost:8080/api/v1/containers/<id>/install
curl http://localhost:8080/api/v1/containers/<id>/install    # poll
```

Two things worth knowing when planning a node:

* **Egress.** SteamCMD talks Steam's own protocol, not only HTTPS, so a node
  behind an HTTPS-only proxy cannot install Steam games.
* **Disk.** Installs land in `DATA_DIR/<server-id>`, and a modern game is tens
  of gigabytes.

## Pre-pull Game Images (optional)

The node pulls a blueprint's image itself the first time a server is created
with it, so this is only a way to get the wait out of the way in advance — a
multi-gigabyte game image can take a while on a slow link.

```bash
sudo ctr -n nexus-panel images pull docker.io/itzg/minecraft-server:latest
sudo ctr -n nexus-panel images pull ghcr.io/parkervcp/steamcmd:debian
```

Pulls run through containerd's own `ctr` client against the socket the node is
configured with, so private registries authenticate exactly as they do for
`ctr` on that host. `ctr` is found on `PATH`, then at `/usr/bin/ctr`,
`/usr/local/bin/ctr` and `/bin/ctr`; set `NEXUS_CTR_BINARY` if it lives
somewhere else.

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
| `nexus_node_container_cpu_usage_millicores` | Per-server CPU use (1000 = one core), sampled every 5 s from its cgroup |
| `nexus_node_container_memory_usage_bytes` | Per-server resident memory, sampled every 5 s from its cgroup |

## Firewall

The node runs its own nftables table, `inet nexus`, for DDoS protection:
per-source SYN and UDP flood meters and a global SYN ceiling on every game
port, an operator blocklist and trusted list, and each running server's
blueprint rules. The installer puts `nftables` in and opens the provisioning
port range in ufw. See `docs/ENTERPRISE.md` for the settings and rule types.

The panel's table only ever *drops* traffic; what is allowed in is still
your host firewall's job:

```bash
# Allow gRPC
sudo ufw allow 8080/tcp

# Allow metrics (internal only)
sudo ufw allow from 10.0.0.0/8 to any port 9090

# Allow game server ports (the installer opens PROVISION_PORT_RANGE)
sudo ufw allow 25565:25665/tcp  # Minecraft
sudo ufw allow 27015:27115/udp  # Source games
```

`nft list table inet nexus` shows what is applied; a server's own view is
its **Firewall** tab in the panel.

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

Each game server's own console output is kept separately, under the data
directory, and is what the panel console reads:

```bash
sudo tail -f /var/lib/nexus-node/runtime/logs/nexus-panel/<server-id>.log
```

This is the first place to look when a server starts and then exits: the game's
own error is in there, while `journalctl` only shows the node's view of it.

## Security Hardening

What the node does for every game server, without configuration:

- Runs it as an unprivileged user (`NEXUS_CONTAINER_UID`), never root, with
  the capability set its blueprint declares (`security.capabilities`,
  starting from the Docker default set), `no_new_privileges`, and the
  standard container seccomp allowlist (`security.seccomp_profile:
  runtime/default`). A game that needs a blocked syscall can set
  `unconfined` or a path to its own profile; nothing shipped does.
- Caps its CPU (`resources.cpu.max`), memory and swap (`resources.memory`),
  process count (`security.pids_limit`), open files and other `ulimits`.
- Measures its disk every minute and refuses to start it, then stops it,
  when it is over `resources.disk.min`.
- Restarts it after a crash, but not forever (`startup.restart`).

What remains yours:

1. **Network**: Run the panel behind a firewall or VPN; expose only game
   ports. Servers share the host's network namespace.
2. **TLS**: Serve the panel over HTTPS; enable mTLS for gRPC.
3. **Updates**: Keep containerd, runc and the kernel updated.
4. **Monitoring**: Set up alerts for unusual activity.
5. **Backups**: Regular backup of `/var/lib/nexus-node`.

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

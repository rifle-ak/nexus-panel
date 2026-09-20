#!/bin/bash
set -eo pipefail

# Nexus Panel - One-Line Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo bash -s --
#
# Non-interactive mode (skip wizard, use defaults or env vars):
#   curl -fsSL ... | sudo NONINTERACTIVE=1 bash -s --
#
# Or if already cloned:
#   sudo bash install.sh

NEXUS_DIR="/var/lib/nexus-node"
NEXUS_CONFIG="/etc/nexus-node"
NEXUS_BIN="/usr/local/bin"
NEXUS_LOG="/var/log/nexus-node"

UPDATE_MODE=false

# Parse flags
for arg in "$@"; do
    case "$arg" in
        --update) UPDATE_MODE=true ;;
    esac
done

# Defaults (can be overridden by env vars or interactive wizard)
GRPC_BIND="${GRPC_BIND:-0.0.0.0:8080}"
METRICS_BIND="${METRICS_BIND:-0.0.0.0:9090}"
WEB_BIND="${WEB_BIND:-0.0.0.0:3000}"
NODE_ID="${NODE_ID:-$(hostname)}"
PANEL_DOMAIN="${PANEL_DOMAIN:-}"
ENABLE_TLS="${ENABLE_TLS:-false}"
ENABLE_AUTH="${ENABLE_AUTH:-false}"
AUTH_PASSWORD="${AUTH_PASSWORD:-}"
LETSENCRYPT_EMAIL="${LETSENCRYPT_EMAIL:-}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info()  { echo -e "\033[1;34m[INFO]\033[0m  $*"; }
ok()    { echo -e "\033[1;32m[OK]\033[0m    $*"; }
warn()  { echo -e "\033[1;33m[WARN]\033[0m  $*"; }
err()   { echo -e "\033[1;31m[ERROR]\033[0m $*"; exit 1; }

# Prompt with a default value. Usage: ask "Question" DEFAULT_VALUE
# Sets REPLY to the user's answer (or default if empty).
ask() {
    local prompt="$1"
    local default="$2"
    if [ -n "$default" ]; then
        printf "\033[1;36m  > \033[0m%s [\033[33m%s\033[0m]: " "$prompt" "$default"
    else
        printf "\033[1;36m  > \033[0m%s: " "$prompt"
    fi
    read -r REPLY </dev/tty || REPLY=""
    REPLY="${REPLY:-$default}"
}

# Yes/no prompt. Usage: ask_yn "Question" [y|n]
# Returns 0 for yes, 1 for no.
ask_yn() {
    local prompt="$1"
    local default="${2:-n}"
    local hint="y/N"
    [ "$default" = "y" ] && hint="Y/n"
    printf "\033[1;36m  > \033[0m%s [\033[33m%s\033[0m]: " "$prompt" "$hint"
    read -r REPLY </dev/tty || REPLY=""
    REPLY="${REPLY:-$default}"
    case "$REPLY" in
        [Yy]*) return 0 ;;
        *)     return 1 ;;
    esac
}

check_root() {
    if [ "$EUID" -ne 0 ]; then
        err "This installer must be run as root (use sudo)"
    fi
}

detect_os() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        OS_ID="$ID"
        OS_VERSION="$VERSION_ID"
    else
        err "Unsupported OS: /etc/os-release not found"
    fi

    case "$OS_ID" in
        ubuntu|debian)
            PKG_MGR="apt-get"
            ;;
        centos|rhel|rocky|alma|fedora)
            PKG_MGR="dnf"
            ;;
        *)
            err "Unsupported OS: $OS_ID. Supported: Ubuntu, Debian, RHEL, CentOS, Rocky, Alma, Fedora"
            ;;
    esac

    info "Detected OS: $OS_ID $OS_VERSION (package manager: $PKG_MGR)"
}

detect_ip() {
    # Try to detect the public IP
    SERVER_IP=$(curl -4 -s --max-time 5 https://ifconfig.me 2>/dev/null || \
                curl -4 -s --max-time 5 https://api.ipify.org 2>/dev/null || \
                hostname -I 2>/dev/null | awk '{print $1}' || \
                echo "unknown")
}

generate_password() {
    # Generate a random 24-char password
    tr -dc 'A-Za-z0-9!@#$%^&*' </dev/urandom 2>/dev/null | head -c 24 || openssl rand -base64 18
}

# ---------------------------------------------------------------------------
# Interactive Setup Wizard
# ---------------------------------------------------------------------------

run_wizard() {
    echo ""
    echo -e "\033[1;36m  ┌──────────────────────────────────────────┐\033[0m"
    echo -e "\033[1;36m  │         Nexus Panel Setup Wizard         │\033[0m"
    echo -e "\033[1;36m  │     Answer a few questions to get        │\033[0m"
    echo -e "\033[1;36m  │     your panel configured perfectly.     │\033[0m"
    echo -e "\033[1;36m  │                                          │\033[0m"
    echo -e "\033[1;36m  │  Press Enter to accept [defaults].       │\033[0m"
    echo -e "\033[1;36m  └──────────────────────────────────────────┘\033[0m"
    echo ""

    # --- Node Identity ---
    echo -e "\033[1;35m  ── Node Identity ──\033[0m"
    ask "Node name" "$NODE_ID"
    NODE_ID="$REPLY"
    echo ""

    # --- Domain / Access ---
    echo -e "\033[1;35m  ── Panel Access ──\033[0m"
    echo -e "  You can access the web panel via IP address or a domain name."
    echo -e "  If you have a domain (e.g. \033[33mpanel.example.com\033[0m), enter it below."
    echo -e "  The installer can automatically set up HTTPS with Let's Encrypt."
    echo ""
    ask "Domain name (or leave blank to use IP: $SERVER_IP)" ""
    PANEL_DOMAIN="$REPLY"

    if [ -n "$PANEL_DOMAIN" ]; then
        echo ""
        echo -e "  Domain: \033[1;32m$PANEL_DOMAIN\033[0m"
        echo -e "  Make sure an \033[33mA record\033[0m points \033[33m$PANEL_DOMAIN\033[0m → \033[33m$SERVER_IP\033[0m"
        echo ""

        if ask_yn "Enable HTTPS with Let's Encrypt (free automatic TLS)?" "y"; then
            ENABLE_TLS="true"
            ask "Email for Let's Encrypt (for renewal notices)" ""
            LETSENCRYPT_EMAIL="$REPLY"
            if [ -z "$LETSENCRYPT_EMAIL" ]; then
                warn "No email provided. Certbot will use --register-unsafely-without-email."
            fi
            # When using TLS via reverse proxy, web panel binds to localhost
            WEB_BIND="127.0.0.1:3000"
        fi
    fi
    echo ""

    # --- Web Panel Port ---
    if [ "$ENABLE_TLS" != "true" ]; then
        ask "Web panel port" "3000"
        WEB_BIND="0.0.0.0:$REPLY"
    fi

    # --- Authentication ---
    echo -e "\033[1;35m  ── Security ──\033[0m"
    if ask_yn "Enable panel authentication (recommended)?" "y"; then
        ENABLE_AUTH="true"
        local generated
        generated=$(generate_password)
        ask "Admin password (auto-generated if blank)" ""
        if [ -z "$REPLY" ]; then
            AUTH_PASSWORD="$generated"
            echo -e "  Generated password: \033[1;33m$AUTH_PASSWORD\033[0m"
            echo -e "  \033[1;31mSave this! It won't be shown again.\033[0m"
        else
            AUTH_PASSWORD="$REPLY"
        fi
    fi

    # Safety guard: never expose an unauthenticated panel to a network. If the
    # operator declined auth but the panel would bind to all interfaces, force
    # it back to loopback so the open panel is only reachable locally.
    if [ "$ENABLE_AUTH" != "true" ] && [ "${WEB_BIND%%:*}" = "0.0.0.0" ]; then
        local port="${WEB_BIND##*:}"
        WEB_BIND="127.0.0.1:$port"
        warn "Authentication is disabled — binding the panel to 127.0.0.1 only."
        warn "Enable authentication (AUTH_ENABLED/AUTH_PASSWORD) before exposing it to a network."
    fi
    echo ""

    # --- gRPC / Metrics Ports ---
    echo -e "\033[1;35m  ── Advanced (ports) ──\033[0m"
    if ask_yn "Customize gRPC and metrics ports?" "n"; then
        ask "gRPC bind address" "$GRPC_BIND"
        GRPC_BIND="$REPLY"
        ask "Metrics bind address" "$METRICS_BIND"
        METRICS_BIND="$REPLY"
    fi
    echo ""

    # --- Confirm ---
    echo -e "\033[1;35m  ── Summary ──\033[0m"
    echo -e "  Node name:      \033[1;37m$NODE_ID\033[0m"
    if [ -n "$PANEL_DOMAIN" ]; then
        if [ "$ENABLE_TLS" = "true" ]; then
            echo -e "  Panel URL:      \033[1;37mhttps://$PANEL_DOMAIN\033[0m"
        else
            local port="${WEB_BIND##*:}"
            echo -e "  Panel URL:      \033[1;37mhttp://$PANEL_DOMAIN:$port\033[0m"
        fi
    else
        local port="${WEB_BIND##*:}"
        echo -e "  Panel URL:      \033[1;37mhttp://$SERVER_IP:$port\033[0m"
    fi
    echo -e "  gRPC:           \033[1;37m$GRPC_BIND\033[0m"
    echo -e "  Metrics:        \033[1;37m$METRICS_BIND\033[0m"
    echo -e "  Authentication: \033[1;37m$([ "$ENABLE_AUTH" = "true" ] && echo "enabled" || echo "disabled")\033[0m"
    echo -e "  TLS:            \033[1;37m$([ "$ENABLE_TLS" = "true" ] && echo "Let's Encrypt" || echo "disabled")\033[0m"
    echo ""

    if ! ask_yn "Proceed with installation?" "y"; then
        echo "  Aborted."
        exit 0
    fi
    echo ""
}

# ---------------------------------------------------------------------------
# Installation steps
# ---------------------------------------------------------------------------

install_system_deps() {
    info "Installing system dependencies..."

    if [ "$PKG_MGR" = "apt-get" ]; then
        apt-get update -qq
        apt-get install -y -qq \
            curl gcc g++ make pkg-config \
            protobuf-compiler libprotobuf-dev \
            libssl-dev \
            containerd runc \
            git > /dev/null
    else
        dnf install -y -q \
            curl gcc gcc-c++ make pkgconf-pkg-config \
            protobuf-compiler protobuf-devel \
            openssl-devel \
            containerd runc \
            git > /dev/null
    fi

    ok "System dependencies installed"
}

install_rust() {
    if command -v rustup &> /dev/null; then
        info "Updating Rust toolchain..."
        rustup update stable --no-self-update > /dev/null 2>&1
    else
        info "Installing Rust toolchain..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable > /dev/null 2>&1
    fi

    # Source cargo env for this session
    export PATH="$HOME/.cargo/bin:$PATH"
    if [ -f "$HOME/.cargo/env" ]; then
        . "$HOME/.cargo/env"
    fi

    RUST_VER=$(rustc --version 2>/dev/null | awk '{print $2}')
    ok "Rust $RUST_VER ready"
}

setup_containerd() {
    info "Configuring containerd..."

    mkdir -p /etc/containerd
    if [ ! -f /etc/containerd/config.toml ]; then
        containerd config default > /etc/containerd/config.toml
    fi

    systemctl enable containerd > /dev/null 2>&1
    systemctl start containerd

    ok "Containerd running"
}

install_cni_plugins() {
    if [ -d /opt/cni/bin ] && [ "$(ls /opt/cni/bin 2>/dev/null)" ]; then
        ok "CNI plugins already installed"
        return
    fi

    info "Installing CNI plugins..."
    CNI_VERSION="v1.4.0"
    ARCH=$(uname -m)
    case "$ARCH" in
        x86_64)  CNI_ARCH="amd64" ;;
        aarch64) CNI_ARCH="arm64" ;;
        *)       err "Unsupported architecture: $ARCH" ;;
    esac

    mkdir -p /opt/cni/bin
    curl -fsSL "https://github.com/containernetworking/plugins/releases/download/${CNI_VERSION}/cni-plugins-linux-${CNI_ARCH}-${CNI_VERSION}.tgz" \
        | tar -xz -C /opt/cni/bin

    ok "CNI plugins installed"
}

check_protoc() {
    if ! command -v protoc &> /dev/null; then
        err "protoc (protobuf compiler) not found. The system dependency install may have failed."
    fi

    local protoc_version
    protoc_version=$(protoc --version 2>/dev/null | grep -oP '\d+\.\d+' | head -1)
    info "protoc version: $protoc_version"
}

build_nexus() {
    local src_dir=""
    local build_log="/tmp/nexus-build-$$.log"
    local branch="${NEXUS_BRANCH:-main}"

    # If we're running from within the repo, use it
    if [ -f "Cargo.toml" ] && grep -q "nexus-panel" Cargo.toml 2>/dev/null; then
        src_dir="$(pwd)"
        info "Building from local source: $src_dir"
    else
        info "Cloning nexus-panel (branch: $branch)..."
        src_dir=$(mktemp -d)
        local clone_log="/tmp/nexus-clone-$$.log"
        # NEXUS_REPO lets you point at an authenticated URL while the repo is
        # private, e.g. https://<token>@github.com/rifle-ak/nexus-panel.git
        local repo_url="${NEXUS_REPO:-https://github.com/rifle-ak/nexus-panel.git}"
        if ! git clone --depth 1 --branch "$branch" "$repo_url" "$src_dir" > "$clone_log" 2>&1; then
            local clone_output
            clone_output=$(cat "$clone_log")
            rm -f "$clone_log"
            err "Could not clone $repo_url (branch: $branch).

$clone_output

If the repository is private, git cannot read it anonymously. Either run the
installer from an existing clone (sudo bash install.sh), or pass an
authenticated URL:

  sudo NEXUS_REPO=https://<token>@github.com/rifle-ak/nexus-panel.git bash install.sh"
        fi
        rm -f "$clone_log"
    fi

    check_protoc

    info "Building nexus-panel (this may take a few minutes)..."
    cd "$src_dir"

    if ! cargo build --release --workspace > "$build_log" 2>&1; then
        echo ""
        err "Build failed. Last 40 lines of output:

$(tail -40 "$build_log")

Full build log: $build_log"
    fi

    # Show final status line
    tail -1 "$build_log"
    rm -f "$build_log"

    # Verify binaries exist
    if [ ! -f target/release/nexus-node ]; then
        err "Build completed but nexus-node binary not found. Check build output."
    fi

    # Backup existing binary before replacing
    if [ -f "$NEXUS_BIN/nexus-node" ]; then
        cp "$NEXUS_BIN/nexus-node" "$NEXUS_BIN/nexus-node.bak"
        info "Previous binary backed up to $NEXUS_BIN/nexus-node.bak"
    fi

    # Install binaries
    cp target/release/nexus-node "$NEXUS_BIN/nexus-node"
    chmod +x "$NEXUS_BIN/nexus-node"

    # nexus-panel CLI is optional (may not exist in all builds)
    if [ -f target/release/nexus-panel ]; then
        cp target/release/nexus-panel "$NEXUS_BIN/nexus-panel"
        chmod +x "$NEXUS_BIN/nexus-panel"
    fi

    ok "Binaries installed to $NEXUS_BIN"
}

setup_directories() {
    info "Creating directories..."

    mkdir -p "$NEXUS_DIR"
    mkdir -p "$NEXUS_CONFIG"
    mkdir -p "$NEXUS_LOG"

    ok "Directories ready"
}

# Add configuration keys this version understands but the existing file does
# not, leaving every value the operator already set exactly as it is.
#
# Without this an upgrade silently skips new settings: the node falls back to
# built-in defaults, and the operator has no way to see there is now a knob.
merge_config() {
    local config_file="$NEXUS_CONFIG/config.env"
    [ -f "$config_file" ] || return 0

    # key=default pairs introduced after the first release. Commented-out
    # entries in the file count as present: an operator who deliberately left
    # something off should not have it reappear.
    local added=0
    local entry key value
    for entry in \
        "UPDATE_CHANNEL=stable" \
        "NEXUS_SNAPSHOTTER=overlayfs" \
        "PROVISION_PORT_RANGE=20000-29999" \
        "NODE_PUBLIC_IP=" \
        "NEXUS_CONTAINER_UID=${NEXUS_GAME_UID:-988}" \
        "NEXUS_CONTAINER_GID=${NEXUS_GAME_GID:-988}"
    do
        key="${entry%%=*}"
        value="${entry#*=}"
        if grep -qE "^[[:space:]]*#?[[:space:]]*${key}=" "$config_file"; then
            continue
        fi
        if [ "$added" -eq 0 ]; then
            printf '\n# Added by install.sh on %s\n' "$(date -u +'%Y-%m-%d %H:%M:%S UTC')" \
                >> "$config_file"
            added=1
        fi
        case "$key" in
            UPDATE_CHANNEL)
                printf '# Which updates this node follows: stable (release tags) or main.\n' \
                    >> "$config_file" ;;
            NEXUS_SNAPSHOTTER)
                printf '# Snapshotter for container root filesystems.\n# ' >> "$config_file" ;;
            PROVISION_PORT_RANGE)
                printf '# Ports handed to servers provisioned by a billing system (docs/WHMCS.md).\n' \
                    >> "$config_file" ;;
            NODE_PUBLIC_IP)
                printf '# The address customers connect to; set it when billing provisions servers here.\n# ' \
                    >> "$config_file" ;;
            NEXUS_CONTAINER_UID)
                printf '# The unprivileged user game servers run as; their files belong to it.\n' \
                    >> "$config_file" ;;
        esac
        printf '%s=%s\n' "$key" "$value" >> "$config_file"
        info "Added $key to config.env"
    done

    [ "$added" -eq 1 ] && ok "Configuration updated with new settings"
    return 0
}

write_config() {
    local config_file="$NEXUS_CONFIG/config.env"

    if [ -f "$config_file" ]; then
        warn "Config already exists at $config_file, skipping"
        merge_config
        return
    fi

    info "Writing configuration..."

    cat > "$config_file" <<EOF
# Nexus Node Configuration
# Generated by install.sh on $(date -u +"%Y-%m-%d %H:%M:%S UTC")

# gRPC server
GRPC_BIND=$GRPC_BIND

# Metrics server
METRICS_BIND=$METRICS_BIND

# Web panel
WEB_BIND=$WEB_BIND
EOF

    # Domain
    if [ -n "$PANEL_DOMAIN" ]; then
        cat >> "$config_file" <<EOF
PANEL_DOMAIN=$PANEL_DOMAIN
EOF
    fi

    cat >> "$config_file" <<EOF

# Containerd
CONTAINERD_SOCKET=/run/containerd/containerd.sock
CONTAINERD_NAMESPACE=nexus-panel

# Data storage
DATA_DIR=$NEXUS_DIR

# Node identification
NODE_ID=$NODE_ID

# Health checks
MIN_DISK_SPACE_BYTES=10737418240
MIN_MEMORY_BYTES=1073741824

# Logging
RUST_LOG=info
LOG_FORMAT=json
EOF

    # TLS config
    # Note: When using Caddy as a reverse proxy (ENABLE_TLS=true with a domain),
    # Caddy handles TLS termination. The nexus-node binary does NOT need its own
    # TLS — it listens on localhost and Caddy proxies HTTPS traffic to it.
    cat >> "$config_file" <<EOF

# TLS (uncomment to enable direct TLS on the gRPC server, without a reverse proxy)
# TLS_ENABLED=true
# TLS_CERT_PATH=$NEXUS_CONFIG/server.crt
# TLS_KEY_PATH=$NEXUS_CONFIG/server.key
# TLS_CA_CERT_PATH=$NEXUS_CONFIG/ca.crt
EOF

    # Auth config
    if [ "$ENABLE_AUTH" = "true" ]; then
        cat >> "$config_file" <<EOF

# Authentication
AUTH_ENABLED=true
AUTH_PASSWORD=$AUTH_PASSWORD
EOF
    else
        cat >> "$config_file" <<EOF

# Authentication (uncomment to enable)
# AUTH_ENABLED=true
# AUTH_PASSWORD=changeme
EOF
    fi

    cat >> "$config_file" <<EOF

# The unprivileged user game servers run as (never root). Their files under
# DATA_DIR belong to it.
NEXUS_CONTAINER_UID=${NEXUS_GAME_UID:-988}
NEXUS_CONTAINER_GID=${NEXUS_GAME_GID:-988}

# Console logs are trimmed past this size (bytes); the last 4 MiB are kept.
# NEXUS_CONSOLE_LOG_MAX_BYTES=33554432
EOF

    cat >> "$config_file" <<EOF

# Billing integration (see docs/WHMCS.md). API keys are for billing systems;
# generate one with: openssl rand -hex 32
# AUTH_API_KEYS=
# The address customers connect to (the node cannot discover it reliably).
# NODE_PUBLIC_IP=
# Ports handed to provisioned servers, inclusive.
PROVISION_PORT_RANGE=20000-29999
EOF

    cat >> "$config_file" <<EOF

# Audit logging (uncomment to enable)
# AUDIT_ENABLED=true
# AUDIT_LOG_FILE=$NEXUS_LOG/audit.log
EOF

    chmod 600 "$config_file"
    ok "Config written to $config_file"
}

# ---------------------------------------------------------------------------
# TLS / Reverse Proxy (Caddy)
# ---------------------------------------------------------------------------

setup_tls() {
    if [ "$ENABLE_TLS" != "true" ] || [ -z "$PANEL_DOMAIN" ]; then
        return
    fi

    info "Setting up HTTPS with Caddy reverse proxy..."

    # Install Caddy
    if ! command -v caddy &> /dev/null; then
        if [ "$PKG_MGR" = "apt-get" ]; then
            apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https > /dev/null 2>&1
            curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg 2>/dev/null
            curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list > /dev/null
            apt-get update -qq > /dev/null 2>&1
            apt-get install -y -qq caddy > /dev/null
        else
            dnf install -y -q 'dnf-command(copr)' > /dev/null 2>&1
            dnf copr enable -y @caddy/caddy > /dev/null 2>&1
            dnf install -y -q caddy > /dev/null
        fi
        ok "Caddy installed"
    else
        ok "Caddy already installed"
    fi

    # Write Caddyfile
    local email_line=""
    if [ -n "$LETSENCRYPT_EMAIL" ]; then
        email_line="    tls $LETSENCRYPT_EMAIL"
    else
        email_line="    tls {
        on_demand
    }"
    fi

    cat > /etc/caddy/Caddyfile <<EOF
# Nexus Panel - Auto-HTTPS reverse proxy
# Managed by nexus-panel installer

$PANEL_DOMAIN {
$email_line

    # Web panel
    reverse_proxy localhost:3000

    # gRPC (if clients connect via domain)
    handle_path /grpc/* {
        reverse_proxy h2c://localhost:8080
    }

    # Security headers
    header {
        X-Content-Type-Options nosniff
        X-Frame-Options SAMEORIGIN
        Referrer-Policy strict-origin-when-cross-origin
        Strict-Transport-Security "max-age=31536000; includeSubDomains"
    }
}
EOF

    # Enable and start Caddy
    systemctl enable caddy > /dev/null 2>&1
    systemctl restart caddy

    # Check if Caddy started
    sleep 2
    if systemctl is-active --quiet caddy; then
        ok "Caddy running — HTTPS will be provisioned automatically for $PANEL_DOMAIN"
    else
        warn "Caddy may not have started cleanly. Check: journalctl -u caddy -n 20"
        warn "Make sure port 80 and 443 are open, and DNS points to this server."
    fi
}

# ---------------------------------------------------------------------------
# Game-server user
# ---------------------------------------------------------------------------

# Game servers run as an unprivileged user, never as root. Their files under
# DATA_DIR belong to it. The default uid (988) is the one Pterodactyl uses, so
# a migrated node keeps its ownership; if that uid already belongs to some
# other account on this host, a fresh system user is created instead and the
# node is told which.
NEXUS_GAME_USER="${NEXUS_GAME_USER:-nexus-game}"
NEXUS_GAME_UID=""
NEXUS_GAME_GID=""

setup_game_user() {
    local existing
    existing=$(getent passwd 988 | cut -d: -f1 || true)

    if id "$NEXUS_GAME_USER" >/dev/null 2>&1; then
        NEXUS_GAME_UID=$(id -u "$NEXUS_GAME_USER")
        NEXUS_GAME_GID=$(id -g "$NEXUS_GAME_USER")
        ok "Game servers run as $NEXUS_GAME_USER (uid $NEXUS_GAME_UID)"
        return
    fi

    if [ -z "$existing" ]; then
        groupadd -r -g 988 "$NEXUS_GAME_USER" 2>/dev/null || groupadd -r "$NEXUS_GAME_USER"
        useradd -r -u 988 -g "$NEXUS_GAME_USER" -d /nonexistent -s /usr/sbin/nologin \
            "$NEXUS_GAME_USER" 2>/dev/null \
            || useradd -r -g "$NEXUS_GAME_USER" -d /nonexistent -s /usr/sbin/nologin "$NEXUS_GAME_USER"
    elif [ "$existing" = "pterodactyl" ]; then
        # A node migrated from Pterodactyl: keep its user and its file ownership.
        NEXUS_GAME_USER=pterodactyl
    else
        warn "uid 988 already belongs to '$existing'; creating $NEXUS_GAME_USER with a free uid"
        groupadd -r "$NEXUS_GAME_USER"
        useradd -r -g "$NEXUS_GAME_USER" -d /nonexistent -s /usr/sbin/nologin "$NEXUS_GAME_USER"
    fi

    NEXUS_GAME_UID=$(id -u "$NEXUS_GAME_USER")
    NEXUS_GAME_GID=$(id -g "$NEXUS_GAME_USER")
    ok "Game servers run as $NEXUS_GAME_USER (uid $NEXUS_GAME_UID)"
}

setup_firewall() {
    # Only configure firewall if ufw is available
    if ! command -v ufw &> /dev/null; then
        return
    fi

    info "Configuring firewall rules..."

    if [ "$ENABLE_TLS" = "true" ]; then
        ufw allow 80/tcp > /dev/null 2>&1   # ACME challenges
        ufw allow 443/tcp > /dev/null 2>&1   # HTTPS
    else
        local port="${WEB_BIND##*:}"
        ufw allow "$port/tcp" > /dev/null 2>&1
    fi

    # gRPC (only if not behind Caddy or binding externally)
    if [ "$GRPC_BIND" != "127.0.0.1:8080" ]; then
        local grpc_port="${GRPC_BIND##*:}"
        ufw allow "$grpc_port/tcp" > /dev/null 2>&1
    fi

    ok "Firewall rules added"
}

# ---------------------------------------------------------------------------
# Systemd Service
# ---------------------------------------------------------------------------

install_systemd_service() {
    info "Installing systemd service..."

    cat > /etc/systemd/system/nexus-node.service <<EOF
[Unit]
Description=Nexus Node Game Server Daemon
After=network.target containerd.service
Requires=containerd.service

[Service]
Type=simple
User=root
EnvironmentFile=$NEXUS_CONFIG/config.env
ExecStart=$NEXUS_BIN/nexus-node
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

# A container can never be granted a higher open-file limit than this process
# holds, and game servers plus SteamCMD want considerably more than the
# systemd default.
LimitNOFILE=1048576

# Security hardening
NoNewPrivileges=false
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$NEXUS_DIR $NEXUS_LOG
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload
    systemctl enable nexus-node > /dev/null 2>&1

    ok "Systemd service installed"
}

start_service() {
    info "Starting nexus-node..."
    systemctl start nexus-node

    # Wait a moment and check status
    sleep 2
    if systemctl is-active --quiet nexus-node; then
        ok "nexus-node is running"
    else
        warn "nexus-node may not have started cleanly. Check: journalctl -u nexus-node -n 20"
    fi
}

# ---------------------------------------------------------------------------
# Final Summary
# ---------------------------------------------------------------------------

print_summary() {
    echo ""
    echo "============================================"
    echo "  Installation Complete"
    echo "============================================"
    echo ""

    if [ -n "$PANEL_DOMAIN" ] && [ "$ENABLE_TLS" = "true" ]; then
        echo -e "  Web Panel: \033[1;32mhttps://$PANEL_DOMAIN\033[0m"
    elif [ -n "$PANEL_DOMAIN" ]; then
        local port="${WEB_BIND##*:}"
        echo -e "  Web Panel: \033[1;32mhttp://$PANEL_DOMAIN:$port\033[0m"
    else
        local port="${WEB_BIND##*:}"
        echo -e "  Web Panel: \033[1;32mhttp://$SERVER_IP:$port\033[0m"
    fi

    if [ "$ENABLE_AUTH" = "true" ]; then
        echo ""
        echo -e "  Log in to the panel with this password:"
        echo -e "  Login password: \033[1;33m$AUTH_PASSWORD\033[0m"
        echo -e "  \033[1;31m  ^ Save this now! It's stored (mode 0600) in $NEXUS_CONFIG/config.env\033[0m"
    else
        echo ""
        echo -e "  \033[1;31mAuthentication is DISABLED — the panel is bound to localhost only.\033[0m"
        echo -e "  Set AUTH_ENABLED=true and AUTH_PASSWORD in $NEXUS_CONFIG/config.env,"
        echo -e "  then set WEB_BIND to 0.0.0.0 before exposing it to a network."
    fi

    echo ""
    echo "  Verify:"
    echo "    curl http://localhost:9090/health"
    echo "    systemctl status nexus-node"
    echo ""
    echo "  Manage:"
    echo "    systemctl stop nexus-node"
    echo "    systemctl restart nexus-node"
    echo "    journalctl -u nexus-node -f"
    echo ""
    echo "  Config:  $NEXUS_CONFIG/config.env"
    echo "  Data:    $NEXUS_DIR"
    echo "  Logs:    journalctl -u nexus-node"
    echo ""
    echo "  CLI tool:"
    echo "    nexus-panel --help"
    echo ""

    if [ "$ENABLE_TLS" = "true" ]; then
        echo "  TLS:"
        echo "    Caddy auto-manages your Let's Encrypt certificate."
        echo "    Config: /etc/caddy/Caddyfile"
        echo "    Logs:   journalctl -u caddy"
        echo ""
    fi

    if [ -n "$PANEL_DOMAIN" ] && [ "$ENABLE_TLS" = "true" ]; then
        echo -e "  \033[1;36mTip: If HTTPS isn't working yet, verify that:\033[0m"
        echo "    1. DNS A record for $PANEL_DOMAIN points to $SERVER_IP"
        echo "    2. Ports 80 and 443 are open in your VPS provider's firewall"
        echo "    3. Caddy is running: systemctl status caddy"
        echo ""
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    echo ""
    echo "============================================"
    echo "  Nexus Panel Installer"
    echo "============================================"
    echo ""

    check_root
    detect_os
    detect_ip

    if [ "$UPDATE_MODE" = true ]; then
        info "Update mode — rebuilding and restarting nexus-node..."
        install_system_deps
        install_rust
        # Stop the running service before replacing the binary to avoid
        # "Text file busy" errors (Linux prevents overwriting a running executable)
        info "Stopping nexus-node before binary replacement..."
        systemctl stop nexus-node 2>/dev/null || true
        setup_game_user
        build_nexus
        # A new binary can need a new unit (resource limits, dependencies) or
        # understand new settings. Updating only the binary leaves those behind
        # on every existing install, silently, forever.
        merge_config
        install_systemd_service
        info "Starting nexus-node..."
        systemctl start nexus-node
        sleep 2
        if systemctl is-active --quiet nexus-node; then
            ok "nexus-node updated and running"
        else
            warn "nexus-node may not have started cleanly. Check: journalctl -u nexus-node -n 20"
            warn "To rollback: sudo cp $NEXUS_BIN/nexus-node.bak $NEXUS_BIN/nexus-node && sudo systemctl restart nexus-node"
        fi
        return
    fi

    # Run interactive wizard unless NONINTERACTIVE is set
    if [ "${NONINTERACTIVE:-}" != "1" ]; then
        # Check if stdin is a terminal (piped installs need /dev/tty)
        if [ -t 0 ] || [ -e /dev/tty ]; then
            run_wizard
        else
            info "No terminal detected, using defaults. Set env vars to customize."
        fi
    else
        info "Non-interactive mode, using defaults/env vars."
    fi

    install_system_deps
    install_rust
    setup_containerd
    install_cni_plugins
    setup_directories
    setup_game_user
    build_nexus
    write_config
    setup_tls
    setup_firewall
    install_systemd_service
    start_service
    print_summary
}

main "$@"

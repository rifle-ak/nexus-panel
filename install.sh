#!/bin/bash
set -e

# Nexus Panel - One-Line Installer
# Usage: curl -sSL https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh | sudo bash
#
# Or if already cloned:
#   sudo bash install.sh

NEXUS_USER="${NEXUS_USER:-nexus}"
NEXUS_DIR="/var/lib/nexus-node"
NEXUS_CONFIG="/etc/nexus-node"
NEXUS_BIN="/usr/local/bin"
NEXUS_LOG="/var/log/nexus-node"

GRPC_BIND="${GRPC_BIND:-0.0.0.0:8080}"
METRICS_BIND="${METRICS_BIND:-0.0.0.0:9090}"
NODE_ID="${NODE_ID:-$(hostname)}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info()  { echo -e "\033[1;34m[INFO]\033[0m  $*"; }
ok()    { echo -e "\033[1;32m[OK]\033[0m    $*"; }
warn()  { echo -e "\033[1;33m[WARN]\033[0m  $*"; }
err()   { echo -e "\033[1;31m[ERROR]\033[0m $*"; exit 1; }

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

# ---------------------------------------------------------------------------
# Installation steps
# ---------------------------------------------------------------------------

install_system_deps() {
    info "Installing system dependencies..."

    if [ "$PKG_MGR" = "apt-get" ]; then
        apt-get update -qq
        apt-get install -y -qq \
            curl gcc g++ make pkg-config \
            protobuf-compiler \
            containerd runc \
            git > /dev/null
    else
        dnf install -y -q \
            curl gcc gcc-c++ make pkgconf-pkg-config \
            protobuf-compiler \
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
    curl -sSL "https://github.com/containernetworking/plugins/releases/download/${CNI_VERSION}/cni-plugins-linux-${CNI_ARCH}-${CNI_VERSION}.tgz" \
        | tar -xz -C /opt/cni/bin

    ok "CNI plugins installed"
}

build_nexus() {
    local src_dir=""

    # If we're running from within the repo, use it
    if [ -f "Cargo.toml" ] && grep -q "nexus-panel" Cargo.toml 2>/dev/null; then
        src_dir="$(pwd)"
        info "Building from local source: $src_dir"
    else
        info "Cloning nexus-panel..."
        src_dir=$(mktemp -d)
        git clone --depth 1 https://github.com/rifle-ak/nexus-panel.git "$src_dir" > /dev/null 2>&1
    fi

    info "Building nexus-panel (this may take a few minutes)..."
    cd "$src_dir"
    cargo build --release --workspace 2>&1 | tail -1

    # Install binaries
    cp target/release/nexus-node "$NEXUS_BIN/nexus-node"
    cp target/release/nexus-panel "$NEXUS_BIN/nexus-panel"
    chmod +x "$NEXUS_BIN/nexus-node" "$NEXUS_BIN/nexus-panel"

    ok "Binaries installed to $NEXUS_BIN"
}

setup_directories() {
    info "Creating directories..."

    mkdir -p "$NEXUS_DIR"
    mkdir -p "$NEXUS_CONFIG"
    mkdir -p "$NEXUS_LOG"

    ok "Directories ready"
}

write_config() {
    local config_file="$NEXUS_CONFIG/config.env"

    if [ -f "$config_file" ]; then
        warn "Config already exists at $config_file, skipping"
        return
    fi

    info "Writing default configuration..."

    cat > "$config_file" <<EOF
# Nexus Node Configuration
# See docs/ENTERPRISE.md for all available options

# gRPC server
GRPC_BIND=$GRPC_BIND

# Metrics server
METRICS_BIND=$METRICS_BIND

# Web panel
WEB_BIND=0.0.0.0:3000

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

# TLS (uncomment to enable)
# TLS_ENABLED=true
# TLS_CERT_PATH=$NEXUS_CONFIG/server.crt
# TLS_KEY_PATH=$NEXUS_CONFIG/server.key
# TLS_CA_CERT_PATH=$NEXUS_CONFIG/ca.crt

# Authentication (uncomment to enable)
# AUTH_ENABLED=true
# AUTH_API_KEYS=changeme

# Audit logging (uncomment to enable)
# AUDIT_ENABLED=true
# AUDIT_LOG_FILE=$NEXUS_LOG/audit.log
EOF

    ok "Config written to $config_file"
}

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
    install_system_deps
    install_rust
    setup_containerd
    install_cni_plugins
    setup_directories
    build_nexus
    write_config
    install_systemd_service
    start_service

    echo ""
    echo "============================================"
    echo "  Installation Complete"
    echo "============================================"
    echo ""
    echo "  Nexus Node is running on this server."
    echo ""
    echo "  Web Panel: http://YOUR_IP:3000"
    echo ""
    echo "  Verify:"
    echo "    curl http://localhost:9090/health"
    echo "    curl http://localhost:9090/metrics"
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
    echo "  Docs: docs/DEPLOYMENT.md, docs/ENTERPRISE.md"
    echo ""
}

main "$@"

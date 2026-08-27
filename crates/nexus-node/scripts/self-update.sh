#!/usr/bin/env bash
#
# Applies a Nexus Panel update, from outside the node process.
#
# Run as a transient systemd unit by the node (see src/selfupdate.rs), because
# it deliberately outlives the process that starts it: replacing the binary
# means killing that process. Nothing here may assume the node is alive, and
# the only channel back to the panel is the status file this writes.
#
# Usage: self-update.sh <status-file> <log-file> <channel>
set -uo pipefail

STATUS_FILE="${1:?status file required}"
LOG_FILE="${2:?log file required}"
CHANNEL="${3:-stable}"

BIN="${NEXUS_BIN:-/usr/local/bin}/nexus-node"
BACKUP="$BIN.bak"
SERVICE="nexus-node"
REPO_URL="${NEXUS_REPO:-https://github.com/rifle-ak/nexus-panel.git}"
RAW_URL="${NEXUS_INSTALL_URL:-https://raw.githubusercontent.com/rifle-ak/nexus-panel/main/install.sh}"

# How long the service gets to prove the new binary works before we conclude
# it does not. A cold start opens the containerd socket and restores state.
HEALTH_TIMEOUT="${NEXUS_UPDATE_HEALTH_TIMEOUT:-60}"

log() {
    printf '%s\n' "$*" | tee -a "$LOG_FILE"
}

# Write the status file the panel polls. Kept to one place so every exit path
# leaves a readable result rather than a job that is running forever.
write_status() {
    local status="$1" exit_code="$2" error="$3"
    local started finished from channel
    started=$(sed -n 's/.*"started_at"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' "$STATUS_FILE" 2>/dev/null | head -1)
    from=$(sed -n 's/.*"from_version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$STATUS_FILE" 2>/dev/null | head -1)
    channel=$(sed -n 's/.*"channel"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$STATUS_FILE" 2>/dev/null | head -1)
    : "${started:=0}" "${from:=unknown}" "${channel:=$CHANNEL}"
    finished=$(date +%s)

    local exit_json="null"
    [ -n "$exit_code" ] && exit_json="$exit_code"
    local error_json="null"
    if [ -n "$error" ]; then
        # Escape the few characters that would break the JSON string.
        error_json="\"$(printf '%s' "$error" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr -d '\n')\""
    fi

    cat > "$STATUS_FILE" <<JSON
{
  "status": "$status",
  "from_version": "$from",
  "channel": "$channel",
  "exit_code": $exit_json,
  "error": $error_json,
  "started_at": $started,
  "finished_at": $finished
}
JSON
}

fail() {
    log "[nexus] update failed: $1"
    write_status "failed" "${2:-}" "$1"
    exit 1
}

log "[nexus] starting update on the $CHANNEL channel"

# ---------------------------------------------------------------------------
# Fetch the installer
# ---------------------------------------------------------------------------

WORK_DIR=$(mktemp -d)
trap 'rm -rf "$WORK_DIR"' EXIT

INSTALLER="$WORK_DIR/install.sh"
if ! curl -fsSL --retry 3 --max-time 120 "$RAW_URL" -o "$INSTALLER"; then
    fail "could not download the installer from $RAW_URL"
fi
if [ ! -s "$INSTALLER" ]; then
    fail "the installer downloaded from $RAW_URL was empty"
fi
log "[nexus] fetched installer"

# The stable channel builds the newest release tag rather than the branch tip.
BRANCH="main"
if [ "$CHANNEL" = "stable" ]; then
    TAG=$(curl -fsSL --max-time 30 \
        -H 'Accept: application/vnd.github+json' \
        "https://api.github.com/repos/rifle-ak/nexus-panel/releases/latest" 2>/dev/null \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
    if [ -n "$TAG" ]; then
        BRANCH="$TAG"
        log "[nexus] stable channel: building $TAG"
    else
        log "[nexus] no release tag found; falling back to main"
    fi
fi

# ---------------------------------------------------------------------------
# Build and install
# ---------------------------------------------------------------------------

# Keep a copy of the binary we are replacing. install.sh makes its own backup,
# but only after a successful build — this one exists before anything is
# touched, so a rollback is always possible.
if [ -f "$BIN" ]; then
    cp -f "$BIN" "$BACKUP" && log "[nexus] backed up current binary"
fi

log "[nexus] building — this takes a few minutes"
set -o pipefail
NEXUS_BRANCH="$BRANCH" NEXUS_REPO="$REPO_URL" bash "$INSTALLER" --update 2>&1 | tee -a "$LOG_FILE"
INSTALL_EXIT=${PIPESTATUS[0]}

if [ "$INSTALL_EXIT" -ne 0 ]; then
    # install.sh replaces the binary only after a successful build, so the
    # node that was running is still the node that is running.
    fail "the installer exited with status $INSTALL_EXIT" "$INSTALL_EXIT"
fi

# ---------------------------------------------------------------------------
# Prove the new binary actually runs
# ---------------------------------------------------------------------------

log "[nexus] waiting for $SERVICE to come back"
deadline=$(( $(date +%s) + HEALTH_TIMEOUT ))
healthy=false
while [ "$(date +%s)" -lt "$deadline" ]; do
    if systemctl is-active --quiet "$SERVICE"; then
        healthy=true
        break
    fi
    sleep 2
done

if [ "$healthy" = true ]; then
    log "[nexus] update complete — $SERVICE is running"
    write_status "succeeded" "0" ""
    exit 0
fi

# The new binary will not start. An operator watching a game host should not
# have to notice this and fix it by hand, so put back what was working.
log "[nexus] $SERVICE did not come back; rolling back"
if [ -f "$BACKUP" ]; then
    systemctl stop "$SERVICE" 2>/dev/null || true
    if cp -f "$BACKUP" "$BIN"; then
        systemctl start "$SERVICE" 2>/dev/null || true
        sleep 3
        if systemctl is-active --quiet "$SERVICE"; then
            log "[nexus] rolled back to the previous binary, which is running again"
            write_status "rolled_back" "$INSTALL_EXIT" \
                "the updated binary would not start; the previous one was restored"
            exit 1
        fi
        log "[nexus] the previous binary did not start either"
        write_status "rolled_back" "$INSTALL_EXIT" \
            "the updated binary would not start, and neither did the previous one — \
check: journalctl -u nexus-node -n 50"
        exit 1
    fi
fi

write_status "failed" "$INSTALL_EXIT" \
    "the updated binary would not start and no backup was available to restore"
exit 1

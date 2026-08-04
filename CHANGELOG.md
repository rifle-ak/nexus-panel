# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security
- The web panel and REST API now require authentication. A password/API-key
  login issues a short-lived session token, and an Axum middleware gates every
  `/api/v1/*` route; it **fails closed** when auth is enabled without a
  configured credential.
- The installer's generated admin credential now actually gates the panel, and
  the installer forces loopback binding when authentication is disabled.
- File-API path traversal is fixed: `..`/root/prefix components are rejected
  before touching the filesystem (previously escapable on writes to
  not-yet-existing paths), with symlink containment as defense in depth.
- All services (`WEB_BIND`, `GRPC_BIND`, `METRICS_BIND`) default to `127.0.0.1`.
- Bumped `anyhow` (1.0.104) and `crossbeam-epoch` (0.9.20) to clear RUSTSEC
  advisories.

### Added
- Container state is persisted to `DATA_DIR/.nexus/state/<id>.json` and restored
  + reconciled against the runtime on startup, so a node restart no longer shows
  zero servers.
- Schedules are persisted and restored on startup; the background schedule
  runner is now started.
- `SECURITY.md`, `CONTRIBUTING.md`, this changelog, and GitHub PR/issue
  templates.

### Changed
- The API routing now uses the correct path-parameter syntax for the pinned
  Axum version (previously every per-server route silently returned 404).
- Manual schedule triggers dispatch real tasks (command / power / backup)
  instead of a no-op callback.
- A containerd connection failure is now fatal; the in-memory mock runtime
  requires an explicit `NEXUS_DEV_MODE=true`.
- The update check compares versions with semver ordering (so `0.10.0` outranks
  `0.9.0`) instead of string comparison.
- CI installs `protoc` in every compiling job, cross-compiles release targets
  with a proper toolchain, and enforces `fmt` + `clippy -D warnings`.

### UI
- Reworked the embedded panel with a "galaxy gaming" theme (deep-space
  backdrop, nebula gradients, glassmorphic cards) and added a login screen.

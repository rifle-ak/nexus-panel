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
- Bumped `anyhow` (1.0.104), `crossbeam-epoch` (0.9.20), and `rand`
  (0.8.7 / 0.9.5) to clear RUSTSEC advisories (the latter for
  RUSTSEC-2026-0097).

### Fixed
- **Umod marketplace adapter refreshed for the current API.** umod.org's
  responses had drifted (display name moved to `title`, `downloads_total` →
  `downloads`, `category` → `category_tags`, `games` → `games_detail`, and the
  details endpoint dropped the `releases[]` history in favor of inline latest-
  release fields), which made search/detail/install fail to parse. The adapter
  now parses the current schema, prefers the RFC3339 `*_atom` timestamps, builds
  the version from the inline latest release, verifies integrity against umod's
  **SHA-1** checksum (the old code compared a SHA-256 to it and always failed),
  and writes the class-named `.cs` file so Oxide/Carbon load it. Verified
  end-to-end against the live API.
- **Codefling** now returns a clear "authentication required" error when its
  Invision Community API rejects unauthenticated requests (`401 NO_API_KEY`),
  and **Lone.Design** reports when it is blocked by Cloudflare's bot challenge
  (`403`), instead of surfacing an opaque failure.

### Added
- **Carbon framework support.** Rust servers can run [Carbon](https://github.com/CarbonCommunity/Carbon)
  as an Oxide-compatible alternative. `ModLoader::Carbon` is a first-class loader,
  the mod-install flow gains an Oxide/Carbon picker (installing to `oxide/plugins`
  or `carbon/plugins` accordingly), and a new `blueprints/rust-carbon.yaml`
  ships a ready-to-use Carbon blueprint.
- **DepotDownloader as a SteamCMD alternative.** `UpdateCheck`/`UpdateApply` gain
  `depot_downloader` variants so blueprints can install/update game files with
  [SteamRE DepotDownloader](https://github.com/SteamRE/DepotDownloader) — useful
  when SteamCMD's download/validation is flaky. The Carbon blueprint uses it.
- **One-click mod install.** `POST /api/v1/containers/:id/mods/install` downloads a
  marketplace mod (fetch + checksum-verify) and installs it into the server's mods
  directory, with the target path jail-checked like the rest of the file API; the
  marketplace UI's mod detail view gains an "Install to server" action (pick a server,
  framework, and target folder). Installing end-to-end depends on the provider adapter
  parsing its API correctly (see ROADMAP note on adapter drift).
- **Real shell console.** A new per-server "Shell" tab and `POST
  /api/v1/containers/:id/exec` endpoint run a one-shot command as a separate
  process inside the container (via `/bin/sh -c`), so operators can run
  arbitrary tooling like `npm install` — distinct from the game console's
  stdin/RCON. Fully working under the dev/mock runtime; the containerd
  implementation (Exec/Start/Wait with FIFO capture, bounded by a timeout)
  ships behind real-runtime validation.
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

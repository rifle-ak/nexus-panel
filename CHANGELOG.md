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
- **Containers could never actually run.** Two defects made every server fail on
  the path from picking a blueprint to pressing start:
  - *The node never pulled an image.* `pull_image` only checked whether an image
    was already present and, when it was not, returned an error telling the
    operator to run `ctr images pull` by hand. The node now pulls the image
    itself, through containerd's own `ctr` client (which handles registry auth,
    platform selection and unpacking, and ships with containerd at every version
    the installer targets).
  - *Created containers had no root filesystem.* A container was registered with
    no snapshot, so starting it asked containerd's shim to run a process in a
    bundle with nothing in it — surfacing as `Container start failed: <uuid>`
    with no explanation. Creating a container now resolves the image's layer
    chain ID (index → manifest → config in the content store), prepares a
    snapshot from it under a lease so the GC cannot reclaim it mid-create, names
    that snapshot on the container record, and passes the resolved mounts to the
    task at start. Deleting a container removes its snapshot instead of leaking
    a full rootfs per server.
- **`Container start failed: <uuid>` said nothing about why.** `StartFailed` and
  `StopFailed` dropped their cause from the message the panel displays; both now
  carry it, so a failure reads e.g. `... exec: "./DayZServer": no such file or
  directory`.
- **Containers had no `/proc`, `/dev` or network.** The generated OCI spec
  omitted the standard Linux mounts, so almost anything a game image runs — a
  shell script, SteamCMD — failed obscurely; and it put every container in a
  private network namespace, which on a node with no CNI plugin means loopback
  and nothing else. Containers now get containerd's default filesystems plus
  masked/read-only kernel paths, and share the host's network namespace (with
  the host's `resolv.conf`, `hosts` and `localtime` bound in) to match how the
  panel allocates ports. Environment, working directory and entrypoint now fall
  back to the image's own config when a blueprint does not set them.
- **Container status could be read from the wrong container.** containerd's task
  list ignores its filter argument and returns every task in the namespace, so
  `inspect` reported whichever task came first — one stopped server could make a
  running one report as stopped. Task lookups are now by container ID.
- **Stopping a container that ignored SIGTERM failed.** After the timeout the
  node sent SIGKILL and immediately deleted the task, which containerd rejects
  with "cannot delete a running process". It now waits for the process to
  actually exit (via the task `Wait` RPC) before reaping it. A game server's main
  process is PID 1 in its namespace and so ignores SIGTERM unless it installs a
  handler, which made this the normal path, not an edge case.
- **Container output went nowhere.** Tasks were created with no stdio at all,
  while the console readers looked for FIFOs in containerd's shim bundle
  directory — a path that never contains any. Containers now log through
  containerd's `file://` stdio to a per-server console log that several viewers
  can follow at once, and stdin is a FIFO the node holds open for the life of the
  task so a console session detaching no longer closes the game server's stdin.
  `StreamLogs` with `follow` set now waits for new output instead of ending as
  soon as it catches up.
- **The shipped Valheim blueprint could not be loaded.** `mods.loader: bepinex`
  failed to deserialize because `snake_case` renders the enum variant as
  `bep_in_ex`; the natural spelling is now accepted. A new test parses and
  validates every blueprint in `/blueprints`, so a shipped blueprint can't drift
  out of the schema again unnoticed.
- **Egg import misdetected games whose egg is named only after the game.** Game
  detection searched an egg's name, description and image but not its startup
  command, so a stock Pterodactyl "Rust" egg (running `./RustDedicated` on a
  generic steamcmd image) imported as `generic` — silently losing Oxide mod
  support and Rust's backup paths. Unambiguous startup binaries are now checked
  first.
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
- **DepotDownloader as an alternative Workshop downloader.** SteamCMD is the
  one that fails opaquely when Steam's content servers misbehave, so
  `STEAM_WORKSHOP_DOWNLOADER` selects `steamcmd` (default), `depot_downloader`
  ([SteamRE DepotDownloader](https://github.com/SteamRE/DepotDownloader), via
  `-pubfile`), or `auto` to try SteamCMD and fall back automatically. A failure
  that exhausts both reports what each tool said, rather than only the first.
  Both tools now run with stdin closed, so a password or Steam Guard prompt
  fails immediately instead of hanging for the full timeout.
- **DayZ/Arma signature keys are installed with the mod.** These engines verify
  mod signatures by default, and a `.bikey` missing from the server's `keys/`
  directory presents as clients being unable to join rather than as a key
  error. Installing a Workshop item now copies its keys there (the whole mod
  tree is scanned, so unconventional layouts work), and the API and UI report
  how many were installed.
- **Mod installs run as a background job.** `POST /api/v1/containers/:id/mods/install`
  now returns a job immediately and `GET` on the same path reports progress,
  mirroring the game-file update executor. Inline installs were fine for a
  50 KB Oxide plugin, but a Workshop mod can be gigabytes — long enough for the
  request to outlive any proxy between the browser and the node. One install at
  a time per server, since concurrent installs would race on the same files.
  **Breaking:** the endpoint's response is now a job snapshot rather than the
  finished install result.
- **Steam Workshop as a mod source.** A `steam_workshop` marketplace adapter
  makes the Workshop a first-class mod location, which is the *only* one for
  games like DayZ, Arma 3, Project Zomboid and Space Engineers. Metadata comes
  from the Steam Web API and downloads run through SteamCMD
  (`+workshop_download_item`), because Workshop content has no public HTTP URL
  and arrives as a directory rather than a single file. The adapter installs an
  item as a mod *folder* — `@ModName` for the DayZ/Arma engines, the Workshop id
  elsewhere — replacing any previous install so files deleted upstream don't
  linger, and reports a content digest over the whole tree. It degrades in
  steps instead of all-or-nothing: without `STEAM_API_KEY` it still resolves a
  pasted Workshop id or `steamcommunity.com` URL (the common case for
  installing a known mod), and without `STEAM_USERNAME` it still downloads from
  Workshops that permit anonymous access. Failures name their fix — a missing
  SteamCMD, an unsatisfied Steam Guard challenge, or the "No subscription"
  refusal that means the game requires an owning account. Configured with
  `STEAM_API_KEY`, `STEAM_USERNAME`/`STEAM_PASSWORD`, `STEAMCMD_PATH`,
  `STEAM_WORKSHOP_CACHE_DIR` and `STEAM_WORKSHOP_TIMEOUT_SECS`; the cache is
  kept between runs so re-downloads are incremental.
- **DayZ blueprint** (`blueprints/dayz.yaml`), wired to the Workshop adapter:
  it declares `marketplaces: [steam_workshop]`, passes `-mod=`/`-serverMod=`
  for the `@ModName` folders the Marketplace installs, and ships DayZ's port
  layout, health/ready checks and backup paths. Also available from the
  Blueprints page.
- **Pterodactyl egg import in the panel.** `POST /api/v1/blueprints/import-egg`
  converts an exported Pterodactyl/Pelican egg into a Nexus blueprint, and the
  Blueprints page gains an import panel: paste an egg, review what was detected
  (game, image, variables, ports) plus a **security report** of risky commands
  found in the egg's scripts, then create a server from the result. Conversion
  is pure — nothing is deployed until the operator acts. Previously this was
  CLI-only (`nexus-panel import` / `convert`), which still works.
- **Per-server blueprint persistence + one-click updates.** The blueprint a
  server was created from is now saved to `DATA_DIR/.nexus/blueprints/<id>.yaml`
  (and removed with the container), so a server's declared configuration
  survives a node restart. `GET /api/v1/containers/:id/update-config` exposes
  the blueprint's `updates.apply` strategy and install directory, and the Update
  tab uses it: it shows what the blueprint declares and offers a single "Run
  blueprint update" button. `POST .../update` now accepts an empty body and
  resolves the strategy (and install dir, from `startup.working_dir`) from the
  blueprint; an explicit body still overrides it, and servers created before
  this change simply fall back to the manual form.
- **On-demand game-file update executor.** A new per-server "Update" tab and
  `POST /api/v1/containers/:id/update` endpoint turn the blueprint `updates`
  strategy into an action: pick SteamCMD or DepotDownloader (with app id,
  branch/beta, depot), and Nexus builds the corresponding command and runs it
  inside the running container. Because game downloads take minutes, it runs as
  a background job (one per server) with a long timeout; the client polls
  `GET .../update` for status/output. The install directory is shell-quoted,
  and the Docker "pull a new image" strategy is rejected (it's a host concern).
  The runtime `exec` now takes a caller-supplied timeout so long updates aren't
  cut off by the interactive shell's 60s bound.
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

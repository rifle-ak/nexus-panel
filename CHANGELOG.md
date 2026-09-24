# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Off-node backups.** Every completed backup is copied to an
  S3-compatible bucket (AWS, Backblaze B2, Wasabi, Cloudflare R2, MinIO,
  Hetzner, DigitalOcean Spaces) as `<prefix>/<server>/<id>.tar.gz` with its
  record beside it. Configured from `NEXUS_BACKUP_S3_*` or the Settings
  page (secret kept in a 0600 file, never returned), with a "Test" that
  writes and reads back an object. `keep_local` off leaves the node with no
  archive; restores and downloads fetch from the bucket. After a rebuild,
  "Sync from bucket" adopts the records there. Deleting a backup or a
  server removes its copies. A failed copy is shown on the Backups tab and
  reported as `backup.failed`.
- **SFTP.** The node serves SFTP itself (`russh`, port 2022 by default):
  the SFTP subsystem only, no shell or exec, every login jailed to one
  server's directory through the web file manager's path checks, uploads
  handed to the game user, no symlink creation, writes refused while over
  the disk allowance. A customer signs in as the server's short id with an
  SFTP password set on the Settings tab; a panel account signs in as
  `account.server` with its own password when it is an admin or holds
  `file.sftp`. Failed logins share the web login throttle and are audited.
  The host key is generated on first run under `DATA_DIR/.nexus`.
- **Branding.** The panel takes the operator's name, tagline, logo, accent
  colour, and billing and support links (`NEXUS_BRAND_*` defaults, editable
  and persisted on the Settings page). The login screen is branded before
  anyone signs in; customers get Billing and Support links in the sidebar.
- **Notifications.** Crashes, crash loops, servers stopped for disk, failed
  backups, failed scheduled tasks and node health changes go to Discord and
  Slack webhooks (formatted), any other URL (JSON) and email over SMTP
  (`lettre`), with a per-channel delivery status and a "Send a test"
  button. Repeats of the same condition on the same server are held for
  ten minutes. A server's owner can add their own webhook on the Settings
  tab and choose which of the server's events they want.
- **Panel accounts and per-server permissions.** Named accounts (Argon2id
  passwords, stored in `DATA_DIR/.nexus/users.json`) sign in with a
  username; admins have the run of the node, others get grants per server
  (`read_only`, `default`, `operator`, `full`, or individual permissions)
  that the API enforces request by request. The operator password from
  the environment still works. Accounts can be disabled, cannot lock
  themselves out, and can change their own password on the Settings page.
- **API keys you can name and revoke.** Minted on the Users page, shown
  once, stored hashed, attributed as `key:<name>` in the audit trail.
- **An audit trail you can read.** The logger keeps its last 2,000 events
  (seeded from the log file at startup) and the Users page shows them with
  a filter and a failures-only switch (`GET /api/v1/audit`).
- **A Settings tab that edits the server.** Name, blueprint variables
  (with the blueprint's rules and `user_editable` honoured, secrets hidden
  from non-admins) and, for the operator, memory, CPU and disk; saving
  rebuilds the container and keeps a provisioned server's billing record
  in step.
- The Blueprints page lists what the node serves instead of a copy kept in
  the frontend; the Sign Out button survives the narrow layout.
- **Backups that behave.** Records now live on disk beside their archives
  (`<id>.json`), so the list survives a restart; archives dropped in by
  hand are adopted and interrupted ones cleaned up. A backup follows its
  blueprint: `pre_backup_command` is sent to a running server first,
  `backups.paths` and `backups.exclude` pick the contents, and
  `backups.retention` deletes the oldest beyond N. Restore verifies the
  checksum, unpacks beside the server directory and swaps the two, so a
  failure leaves the server untouched; files come out owned by the game
  user; a running server is refused unless the caller asks to stop it.
  Backups can be downloaded. Deleting a server deletes its backups and
  schedules.
- **Schedules in five-field cron, with time zones.** `0 4 * * *` is read as
  everyone writes it (six and seven fields still work) and in the
  schedule's IANA zone, so 4 a.m. is the operator's. The runner starts
  each due schedule on its own task, computes its next run at once, and
  never stacks a schedule on itself; a task's `time_offset` used to hold
  every other schedule on the node. The first failing task is recorded as
  `last_error` and shown; schedules can be paused and resumed.
- **Files in and out of the panel.** Upload (streamed, atomic, refused
  when it would exceed the server's disk allowance), download, compress
  to `.zip`/`.tar.gz`, and extract, all on the Files tab.

### Fixed
- **Selective backups were empty.** A blueprint listing `/world` produced
  an archive with nothing in it: the walk filter dropped the root
  directory because `/` did not match `/world`, and never descended. The
  filter now keeps the way to each include, and paths that match nothing
  fail the backup instead of writing an empty archive.
- **The console shows the server's output.** It never did: the Console tab
  had a terminal and a command box, and nothing came back. It now loads
  the last 64 KiB of the log (`GET /api/v1/containers/:id/console`) and
  follows new output as server-sent events
  (`…/console/stream`), reconnecting with backoff when a server restarts.
- **Per-server resource usage from cgroups.** The runtime reads each
  running server's cgroup (v2 or v1, found from the task's pid): CPU time,
  memory with and without page cache, the memory limit, process count and
  block I/O. A monitor (`stats.rs`) samples every 5 seconds, turns the
  counters into rates, keeps ten minutes of history, and feeds the
  Prometheus gauges that were registered but never set. Meters under the
  server's status bar, CPU/memory columns on the server list,
  `GET /api/v1/containers/:id/stats`, and `usage` on the container JSON.
- **An Analytics page with real numbers**: node CPU, memory, swap and load
  with ten-minute sparklines, every running server's share, and the health
  checks (`GET /api/v1/node/stats`).
- **Health checks that measure something.** `containerd` was "the socket
  file exists" and `disk` and `memory` always passed. Now containerd must
  answer a version request within five seconds, free space on the data
  directory's filesystem is compared with `MIN_DISK_SPACE_BYTES` and
  available memory with `MIN_MEMORY_BYTES` (below the minimum fails;
  below twice it warns), a disabled firewall warns with its reason, and a
  server that has crashed three or more times warns as crash-looping.
- **A real firewall.** The unfinished XDP module is replaced by an nftables
  firewall (`firewall.rs`) that owns one table, `inet nexus`, and keeps it
  in step with what runs. Node-wide: a blocklist (timed or permanent) and a
  trusted list, invalid-state drops, per-source SYN and UDP flood meters
  and a global SYN ceiling on every game port, plus SYN-cookie and
  conntrack sysctls. Per server: a chain of the blueprint's
  `security.firewall_rules` (connection rate, packet size, allow and block
  CIDRs), attached to its ports through verdict maps when it starts and
  removed when it stops, with per-rule counters. Every change is one
  atomic `nft -f` transaction. `NEXUS_FIREWALL=auto|on|off` and
  `NEXUS_FIREWALL_{SYN_PER_SOURCE,SYN_GLOBAL,UDP_PER_SOURCE,TRUSTED,SYSCTL}`
  tune it; the installer installs `nftables` and opens the provisioning
  port range.
- **Security page and Firewall tab.** Operators see the protections with
  live counters, the blocklist and trusted list, and each attached server;
  a server's owner edits its own rules and has a one-click ban. Endpoints:
  `/api/v1/firewall*` and `/api/v1/containers/:id/firewall*`.

### Removed
- `xdp_firewall.rs`, which compiled a placeholder and filtered nothing.

### Security
- **Archive extraction could write anywhere on the node.** A zip or tar
  entry named `../../etc/cron.d/x` was joined to the extraction directory as
  given, so a customer-uploaded archive could drop a file anywhere the node
  can write. Zip entries now go through the archive library's traversal
  check (`enclosed_name`) and tar entries through `unpack_in`; an entry
  that would escape is skipped and logged. Tests cover both.
- **The gRPC `DownloadFile` RPC read any file on the node.** It joined the
  raw request path under the server directory, stripping only leading
  slashes, so `../../etc/shadow` was served. It now resolves through the
  same jail every other file operation uses.
- **Uploads were buffered whole in memory** before being written; a world
  the size of RAM took the node down. They stream to disk.
- **Copying a directory followed symlinks.** A link the game planted to a
  path outside its directory became a copy of that path's contents. Links
  are recreated as links.
- **The panel password could be guessed at full speed.** The login endpoint
  had no attempt limit. Ten failures from one address, or two hundred
  node-wide, in fifteen minutes now answer `429` with `Retry-After`;
  every failure and lockout is an audit event. Behind the installer's Caddy
  the address is the real client's (`X-Forwarded-For` is trusted only from
  a local or private-network proxy).
- **Cross-site scripting through file names.** The panel's HTML escaper left
  quotes alone while most of the UI interpolates into single-quoted `onclick`
  attributes; a file named `x');alert(1);('` ran script. Quotes are escaped.
- **Blueprint variable rules are enforced.** A `MAX_PLAYERS=-1` or a port
  outside a variable's declared range reached the game unchallenged; a
  provisioning request that breaks a rule is refused with the rule.

### Added
- **Audit logging covers what the panel does.** With `AUDIT_ENABLED=true`
  the node records logins (success, failure, lockout), SSO sign-ins,
  container create/start/stop/restart/suspend/unsuspend/delete, and file
  writes, deletes and renames — with the acting session, the source
  address, the target server and the outcome. Before this the audit log
  held two events per process lifetime: node started, node stopped.

### Fixed
- **File "Edit" was broken for password-authenticated admins**: it used a
  bare `fetch` with no session header and got `401` on every click.
- **Six of seven shipped blueprints could not start.** Startup arguments were
  passed to the game verbatim, so a CS2, Rust, Valheim, Palworld or DayZ
  server was launched with a literal `{{SERVER_PORT}}` on its command line
  and each multi-word argument (`-port 27015`) as one token. Startup
  commands are now rendered with the server's variables and split with shell
  rules, so `+hostname "{{SERVER_NAME}}"` is two arguments and a name with
  spaces stays one. Java servers get the JVM flags their blueprint declares
  (`performance.jvm`: heap sizes, Aikar's collector settings), placed before
  `-jar`; a Minecraft server sold 8 GiB no longer runs on the JVM's default
  quarter of it.
- **A crashed server showed as running forever.** Nothing watched a game
  process exit: the panel, the API and the metrics all served the state
  recorded at start. Every start now has a watcher that records the exit
  (`Stopped` for a clean exit, `Failed` with the exit code otherwise) and a
  `crash_count` on the server.
- **Stops were unclean kills.** A blueprint's `pre_stop` console commands
  (`save-all`, `stop`, `quit`) were never sent; every stop was SIGTERM then
  SIGKILL. The shutdown sequence now runs first and the runtime only forces
  the issue if the game ignores it. Blueprints may also name a `stop_signal`
  (Valheim saves on `SIGINT`) and a `stop_timeout`.
- **A timed-out shell command kept running inside the customer's container**
  and leaked a FIFO directory in `/tmp`. It is killed and reaped.
- **A container containerd had lost stayed unstartable forever.** On restart
  the node rebuilds it from the stored blueprint. A server whose task was
  reaped while the node was down is restored as stopped, not "created".
- **A suspended server could be started** from the panel or API, which is
  not what a billing suspension means.
- Imported Pterodactyl eggs carry their `stop` command (or `^C`) into the
  blueprint's shutdown sequence.

### Added
- **Crash recovery.** A server that exits with a failure is started again
  after `startup.restart.delay` (5 s), up to `max_retries` (5) times within
  `reset_after` (10 min); past that it stays down and says so. A clean exit
  is treated as intentional. Settable per blueprint; on by default.
- **Game servers no longer run as root.** The process runs as an
  unprivileged user (`NEXUS_CONTAINER_UID`/`GID`, default 988, the id
  Pterodactyl uses so a migrated node keeps its ownership); the installer
  creates the account. Server directories belong to that user, files the
  panel writes are handed to it, and a server created before this change is
  re-owned on its next start. The one-shot install container still runs as
  root, because egg scripts expect to, and hands its output over when done.
- **Containers are confined.** The blueprint `security` block, parsed and
  ignored until now, is applied: capabilities (`drop: [ALL]`, `add: […]`)
  from the Docker default set, `no_new_privileges`, `read_only_root`, and a
  seccomp allowlist (`runtime/default`, the standard container profile: no
  `mount`, `bpf`, `setns`, module loading, or new namespaces). A
  `pids_limit` (1024) stops a fork bomb at one server. The hostname is the
  server's id rather than `container` for every server on the node.
- **Resource limits mean what they say.** `resources.cpu.max` is now a hard
  cap (CPU quota), not just a weight; `memory.swap` is applied (and is zero
  by default, so a server sold N GiB gets N GiB of RAM, not part of it in
  swap); every `performance.kernel.ulimits` key is honoured, not only
  `nofile`.
- **Disk allowances are enforced.** Each server's directory is measured every
  minute (`disk_used_bytes` in the API and the panel header). A server over
  its `resources.disk.min` cannot be started, and a running one that stays
  over for a minute is stopped, with the reason logged. A quota you report
  but never enforce is a support ticket at 3 a.m.
- **Console logs are capped.** A log past 32 MiB (`NEXUS_CONSOLE_LOG_MAX_BYTES`)
  is trimmed to its last 4 MiB; a viewer following it picks up from the
  trim. Before this a chatty server grew its log without bound.
- The server header shows crashes and disk usage, and refreshes every cell
  (it used to update only the status badge, so uptime and PID froze at
  page load).

### Added
- **WHMCS provisioning module.** `whmcs/modules/servers/nexuspanel` is a
  standard WHMCS server module: a paid order creates a server from the
  product's blueprint with the product's memory, CPU, disk, slots and
  variables; an overdue invoice suspends it; payment unsuspends and restarts
  it; an upgrade rebuilds it with the new limits; a cancellation removes it.
  Customers see status, address and ports on the service page with Start /
  Stop / Restart, and an **Open game panel** button that signs them in.
  Configurable options (`Memory (GB)`, `CPU Cores`, `Player Slots`, …) and
  custom fields (`Server Name`, any `UPPER_CASE` field as a variable) override
  the product. Disk usage flows back through WHMCS's usage cron. It has its
  own test suite (`php whmcs/tests/run.php`) that runs in CI on PHP 8.1 and
  8.3. Setup: `docs/WHMCS.md`.
- **Provisioning API.** `/api/v1/provision/servers` creates a server from a
  blueprint id plus resource overrides, idempotently on the billing system's
  service id — a retry after a timeout finds the server it already made.
  Ports are allocated from `PROVISION_PORT_RANGE` (default `20000-29999`):
  every `{{VARIABLE}}` port a blueprint declares gets a free value, and a
  blueprint that pins a literal port already in use is refused rather than
  left to fail at start. Package changes rebuild the container with new
  limits while keeping the customer's ports and files; termination is
  idempotent; a usage endpoint reports disk. Records live under
  `DATA_DIR/.nexus/provision`. `GET /api/v1/blueprints` lists what the node
  ships. `NODE_PUBLIC_IP` tells customers where to connect.
- **Server-scoped sessions.** A billing system mints a one-time sign-in link
  (`POST /api/v1/provision/sso`, redeemed at `GET /sso/:token`); the browser
  gets an `HttpOnly` cookie session that can act on that one server and
  nothing else — the node answers `403` for everything at node level, for
  other servers, and for deleting the server, which only the billing system's
  termination does. Links work once and expire in a minute. The UI follows
  the scope: a customer sees their servers, the marketplace, and no admin
  pages or buttons. `GET /api/v1/auth/me` reports a session's scope.
- **`ContainerManager::reconfigure_container`** replaces a server's blueprint
  and recreates its runtime container to match, without touching its files
  or install state.

### Removed
- **The `nexus-whmcs` crate.** It was a Rust client for the WHMCS API that
  nothing used and that had the integration backwards: WHMCS calls a
  provisioning module when an order changes, the module calls the panel —
  not the other way round. Its presence made the README's "WHMCS integration
  (backend implemented)" claim true only in the sense that some code existed.
  The PHP module above is the integration.

### Added
- **The panel can update itself.** Settings gains an Update panel: what this
  node is running, what its channel has available, and a button that applies
  it. The updater runs in its own transient systemd unit rather than as a child
  of the node, because applying an update means restarting the node — a child
  would be killed half-way through replacing the binary. Its status and log
  live on disk for the same reason: the process that starts the job is not
  around to finish it, and the panel reads the result back after the node
  returns. The UI follows the update across that restart instead of reporting
  the node's absence as a failure.
- **Failed updates roll back on their own.** If the new binary will not start,
  the previous one is restored and started, and the panel says so. An update
  that leaves a game host down until someone notices is not an update anyone
  should press.
- **Update channels.** `UPDATE_CHANNEL=stable` (the default) follows published
  release tags; `UPDATE_CHANNEL=main` follows the branch, which is what the
  installer's default build already tracks.
- **The binary knows what it was built from.** `build.rs` stamps the git
  commit, its date, and whether the tree was dirty, so a node tracking `main`
  can report how many commits behind it is. Builds without git (a source
  tarball) say `unknown` rather than failing.

### Fixed
- **The update check reported a permanent phantom update.** It compared the
  crate version against the latest release tag, and the manifest still said
  `0.1.0` while `v0.1.1` was published — so every node claimed an update was
  available forever, including one running the newest code. The manifest now
  matches the published release, and an update check that cannot reach a
  conclusion says so instead of guessing "up to date", which is the one answer
  that actively misleads.
- **`install.sh --update` silently skipped everything but the binary.** It
  never refreshed the systemd unit or the config, so changes to either — the
  raised `LimitNOFILE`, any new setting — never reached an existing node, on
  any update, ever. Update mode now reinstalls the unit and merges genuinely
  new config keys, leaving every value the operator set alone: a changed value
  stays changed, and a key they commented out stays commented out. Customise
  the unit with `systemctl edit nexus-node`, since the unit file itself is
  now managed.

### Added
- **Game files are now installed.** A blueprint's image supplies the tooling a
  game needs — a JVM, a .NET runtime, the 32-bit libraries SteamCMD links
  against — but never the game itself, and nothing fetched it:
  `ghcr.io/parkervcp/steamcmd:debian` does not even contain SteamCMD. Every
  server therefore came up with an empty directory and a startup command that
  did not exist. Creating a server now runs its install first, as a one-shot
  container with the server's directory mounted into it, and the server is not
  startable until that succeeds.
  - **Where the install comes from.** An explicit `install:` block in the
    blueprint (image, entrypoint, script, server directory, timeout), else the
    `startup.lifecycle.pre_start` actions the shipped blueprints declare, else
    the `updates.apply` strategy — installing into an empty directory and
    updating in place are the same operation. A blueprint that declares none of
    these needs no install and its server starts immediately.
  - **The tools get installed too.** The shipped blueprints call
    `./steamcmd/steamcmd.sh`, which no game image provides; the generated
    script fetches SteamCMD (and DepotDownloader, for the Carbon blueprint)
    into the server directory first, the way Pterodactyl's own install
    container does. Both spellings work: the literal path and a bare
    `steamcmd` on `PATH`.
  - **`{{VARIABLE}}` placeholders are substituted** in install scripts, so the
    Paper blueprint's templated download URL resolves instead of 404ing on a
    literal `{{MC_VERSION}}`. An unknown placeholder is left visible rather
    than blanked, so a missing variable is legible in the failure.
  - **Progress is visible.** `POST /api/v1/containers/:id/install` starts one
    and `GET` polls it; the panel gains an Install tab with live output, the
    server's game-file state in its status bar, and an install button in place
    of a Start that could only fail. A server created with auto-start begins
    once its files are in place.
  - Installs are one at a time per server, are killed at the blueprint's
    timeout (one hour by default), and abort on the first failing step, so a
    server is never reported ready with half a game in it.

### Changed
- **Imported eggs keep their installation script.** The egg importer only
  carried the script across when it mentioned `steamcmd`, silently discarding
  it otherwise — importing a game that could never install itself. Every egg's
  script now becomes the blueprint's `install` block, along with the installer
  image and entrypoint it names, and Pterodactyl's `/mnt/server` convention.
- **Containers get a usable open-file limit.** The OCI spec pinned
  `RLIMIT_NOFILE` at 1024, below what SteamCMD asks for before a single player
  connects; blueprints can now set it (`performance.kernel.ulimits.nofile`) and
  the default is 65536, capped at what the node process itself holds — asking
  for more than that makes runc refuse the container outright. The installer's
  systemd unit raises the node's own limit to match.

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

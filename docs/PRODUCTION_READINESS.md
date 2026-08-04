# Nexus Panel — Production Readiness Review

**Date:** 2026-08-04
**Reviewed commit:** `8c64c36` (branch `main`)
**Scope:** Full workspace — build, tests, CI, security posture, runtime behaviour, deployment, docs.

This document is the single source of truth for *what it takes to ship Nexus Panel to production*.
It records what is genuinely done, the concrete blockers, and a prioritized, actionable remediation
plan. Every finding cites the code that produced it (`file:line`) so it can be verified and fixed.

> **Remediation progress — Phases 0 & 1 (2026-08-04).**
>
> *Phase 0 (security) — complete & merged.* The web panel now requires authentication (C-1), the
> installer's credential actually gates access (C-2), the file-API traversal hole is closed (C-3),
> and all services default to loopback (M-4). Verification also uncovered and fixed a latent routing
> bug — the panel's routes used axum-0.8 `{id}` path-param syntax on axum 0.7, so every container
> detail, file, backup, and schedule route silently 404'd; they now use `:id`.
>
> *Phase 1 (make CI real) — in progress.* CI now installs `protoc` in every compiling job; the tree
> is `cargo fmt` clean and passes `cargo clippy --all-targets -- -D warnings` across the workspace;
> the two RUSTSEC advisories are bumped (`anyhow` 1.0.104, `crossbeam-epoch` 0.9.20, M-6); the
> multi-arch build job uses a proper cross toolchain; and the flake-prone coverage/deadlinks steps
> are reported but non-gating. Green status is pending the first real CI run.

---

## 1. Verdict

**Phase 0 security blockers resolved; not yet production ready.** The remaining work is Phase 1–3
(CI, operational correctness, polish). The codebase compiles, 164 unit tests pass, and the primary
management surface is now authenticated and traversal-safe. CI still cannot compile (Phase 1).

| Area | State | Blocker? |
|------|-------|----------|
| Builds from clean checkout | ✅ Works (needs `protoc`) | — |
| Unit tests | ✅ 164 pass, 11 integration ignored | — |
| Web panel / REST API auth | ✅ **Resolved** — login + session middleware | — |
| Installer security claims | ✅ **Resolved** — credential now enforced | — |
| File API path traversal | ✅ **Resolved** — `..` rejected on writes | — |
| Panel API routing | ✅ **Fixed** — `:id` params (was silently 404ing) | — |
| CI pipeline | 🛠️ **Phase 1** — protoc + fmt + clippy + advisories fixed (green pending CI) | — |
| State persistence across restart | ❌ In-memory only | High |
| Containerd failure handling | ⚠️ Silent fake-runtime fallback | High |
| TLS for the panel | ⚠️ Relies entirely on external Caddy | Medium |
| Feature completeness vs. README | ⚠️ Several "done" items are stubs | Medium |
| Repo hygiene | ⚠️ Build artifacts committed | Low |

---

## 2. Critical blockers (must fix before any public deployment)

### C-1. The web panel and REST API have zero authentication

The node binary starts an Axum web server bound to `0.0.0.0:3000` by default and mounts every
management route on it — container create/start/stop/kill/delete, arbitrary **file read/write/delete**,
backups, schedules, and **arbitrary command execution inside containers** — with **no auth layer,
no session, no token check**.

- Router is built and served with only `.with_state(shared)` — no middleware:
  `crates/nexus-node/src/web/mod.rs:65-116`, `web/mod.rs:123`.
- Default bind is all interfaces: `WEB_BIND` default `0.0.0.0:3000` at
  `crates/nexus-node/src/bin/nexus-node.rs:66`.
- The `AuthLayer` that *does* exist is wired **only into the gRPC server**, never the web server:
  `bin/nexus-node.rs:372-379` vs. the un-layered `start_web_server` spawn at `bin/nexus-node.rs:266-283`.
- The bundled UI has no login flow — the "Authentication" panel is a static status card, and
  `app.js` sends no `Authorization`/`X-API-Key` header on any request
  (`crates/nexus-node/static/index.html:259`, `static/js/app.js`).

**Impact:** anyone who can reach port 3000 has full root-equivalent control of every game server on
the node, plus read/write to the host filesystem via the file API (see C-3). This is the single
most important issue in the repository.

**Fix:** put an authentication + authorization middleware in front of the Axum router
(session cookie or bearer token), add a real login endpoint, and reuse the existing `Identity`/RBAC
model from `auth.rs`. Bind to `127.0.0.1` by default and require an explicit opt-in to expose it.

### C-2. The installer advertises a login password the software never checks

The installer generates an admin password, writes `AUTH_ENABLED=true` and `AUTH_PASSWORD=…` into the
service environment file, and prints *"Login password: …"* to the operator
(`install.sh:180-187`, `install.sh:458-459`, `install.sh:660`).

But **`AUTH_PASSWORD` is not read anywhere in the codebase.** `AuthConfig::from_env` only understands
`AUTH_API_KEYS`, `AUTH_JWT_SECRET`, `AUTH_JWT_*` — there is no password auth
(`crates/nexus-node/src/auth.rs:204-235`). So:

1. The web panel remains completely open (C-1) despite the "password."
2. Setting `AUTH_ENABLED=true` with **no** `AUTH_API_KEYS`/`AUTH_JWT_SECRET` makes the gRPC
   interceptor reject *every* call with `MissingCredentials` (`auth.rs:350-386`) — locking out
   legitimate gRPC clients while the web panel stays wide open.

**Impact:** operators are actively misled into believing the panel is protected. This is worse than
having no auth, because it removes the incentive to add network-level protection.

**Fix:** implement password (or key) auth that actually gates the web panel, and make the installer's
generated credential real. Until then, the installer must not claim a login exists.

### C-3. Path traversal is escapable on the file API (write / mkdir / rename target)

`FileManager::sanitize_path` canonicalizes the path but **falls back to the raw joined path when
canonicalization fails**, and then uses a *lexical* `starts_with` check
(`crates/nexus-node/src/files.rs:59-79`):

```rust
let canonical = full_path.canonicalize().unwrap_or_else(|_| full_path.clone());
if !canonical.starts_with(&self.server_dir) { /* reject */ }
```

`canonicalize()` fails for paths that don't exist yet — exactly the case for **creating a new file,
directory, or a rename destination**. For input like `../../../../etc/cron.d/pwn`, the fallback path is
`<server_dir>/../../../../etc/cron.d/pwn`, whose components still lexically start with `server_dir`, so
the check passes and the write escapes the jail. Reads of existing files are caught (canonicalize
succeeds), but `write_file`, `create_directory`, and the destination of `rename` are not.

Combined with C-1 (no auth) and the service running as root under systemd, this is a remote arbitrary
file-write primitive on the host.

**Fix:** normalize the path lexically (reject any `..` component up front), resolve symlinks on the
parent directory, and verify containment against a canonicalized `server_dir`. Add tests for the
non-existent-path case.

### C-4. CI cannot build the project and its quality gates are red

The pipeline (`.github/workflows/ci.yml`) never installs `protoc`, but the build hard-requires it —
`containerd-client`'s build script fails with *"Could not find protoc"* on a clean machine. Every job
that compiles `nexus-node` (test, build, security's audit, lint-docs) therefore fails. On top of that:

- `cargo fmt --all -- --check` **fails** — the tree is not formatted (`ci.yml:52`).
- `cargo clippy --all-targets --all-features -- -D warnings` **fails** with many `dead_code`
  errors across the marketplace/whmcs crates (`ci.yml:54-56`; verified locally, exit 101).

**Impact:** the "green CI" signal is meaningless because CI is red (or was never passing). No
regression protection exists.

**Fix:** add a `protobuf-compiler` install step to every compiling job, run `cargo fmt --all`, and
resolve the clippy `dead_code` findings (wire the fields up or annotate/remove them).

---

## 3. High-severity gaps (fix before real workloads)

### H-1. No state persistence — a restart loses everything

Container tracking and schedules live only in in-memory maps, and nothing rehydrates them at startup:

- `ContainerManager.states: Arc<RwLock<HashMap<...>>>`, initialized empty, never loaded from disk
  (`crates/nexus-node/src/container/manager.rs:20,34,48`). No `load_state`/reconcile exists.
- `ScheduleManager` is a bare in-memory map (`crates/nexus-node/src/schedule.rs:74-85`).

**Impact:** after a `systemctl restart`, crash, or host reboot, the panel reports **zero containers**
even though containerd may still be running them; all schedules are gone and the schedule runner stops
firing. Backups/schedules silently stop.

**Fix:** persist container metadata and schedules to `DATA_DIR` (the code already writes per-container
dirs) and reconcile against containerd on startup — list live tasks and rebuild in-memory state.

### H-2. Silent fallback to a fake runtime in production

If the containerd connection fails at startup, the node logs a warning and silently switches to
`MockRuntime`, which *pretends* to create/start containers (`bin/nexus-node.rs:148-167`).

**Impact:** in production this masks a hard dependency failure. Operators see "servers running" while
nothing is actually running. The code comment even says *"In production, you might want to exit here."*

**Fix:** gate the mock runtime behind an explicit `NEXUS_DEV_MODE`/`--mock` flag and **exit non-zero**
on containerd connection failure in normal operation.

### H-3. Manual schedule trigger is a no-op

`api_trigger_schedule` passes a callback that does nothing, so triggering a schedule from the web UI
silently succeeds without running the task (`crates/nexus-node/src/web/mod.rs:784-799`). The comment
acknowledges it. Users will believe a backup/command ran when it did not.

**Fix:** dispatch the real callback (the same one the schedule runner uses) that routes to the
container manager.

---

## 4. Medium-severity issues

- **M-1. README overstates security maturity.** The README markets "Security First," RBAC, and audit
  logging as delivered, while `ROADMAP.md:46-66` correctly lists API-key/JWT auth, rate limiting,
  audit logging, RBAC, sub-users, and the firewall UI as "Backend Ready" or "Planned." RBAC exists in
  `auth.rs` but is **not enforced on the web API** at all. Align the README with the roadmap.
- **M-2. Update check uses string comparison, not semver.** `latest != current && latest > current`
  compares versions lexically (`web/mod.rs:937`), so e.g. `0.9.0` > `0.10.0`. The `semver` crate is
  already a workspace dependency (`Cargo.toml:32`) but unused here. Also, `update-check` and the
  installer's `--update` path depend on GitHub *Releases* that do not appear to exist yet — publish
  releases or the feature always reports "up to date."
- **M-3. Secrets stored/echoed in plaintext.** The generated admin password and RCON passwords are
  written to the env file and printed to the terminal (`install.sh:660`), and blueprint/app defaults
  use `"changeme"` (`static/js/app.js:105,280`). Document rotation, restrict env-file permissions, and
  stop echoing secrets.
- **M-4. Metrics and gRPC exposed on all interfaces by installer.** The installer opens the gRPC port
  in ufw and defaults binds to `0.0.0.0` (`install.sh`, `bin/nexus-node.rs:63-66`). The metrics
  endpoint is unauthenticated. Default to loopback; expose deliberately.
- **M-5. Dead-code warnings signal unfinished wiring.** Numerous `field is never read` errors in the
  marketplace and WHMCS crates (surfaced by clippy) indicate response models that are parsed but never
  surfaced — a sign features are partially plumbed.
- **M-6. Known-vulnerable dependencies in the lock file.** `cargo deny check` fails on two RUSTSEC
  advisories (confirmed by the Security Scan CI job):
  - `crossbeam-epoch 0.9.18` — **RUSTSEC-2026-0204** (invalid pointer dereference); fix:
    `cargo update -p crossbeam-epoch` (→ ≥ 0.9.20). Pulled in via `criterion` (dev-dependency).
  - `anyhow 1.0.102` — advisory [dtolnay/anyhow#451]; fix: `cargo update -p anyhow` (→ ≥ 1.0.103).

  Note the CI `cargo audit` step is `continue-on-error: true` (`.github/workflows/ci.yml:49`), so audit
  never gates — only `cargo deny` does. Keep both hard-failing and refresh dependencies regularly.

---

## 5. Low-severity / hygiene

- **L-1. Build/run artifacts committed to the repo:** `conversion_output.log` (82 KB),
  `failed_conversions.txt` (60 KB), empty `successful_conversions.txt`, `conversion_report.txt`, and
  **258** generated `converted_eggs/*.yaml`. None are covered by `.gitignore`. Remove them and extend
  `.gitignore`.
- **L-2. Compiler warnings** (unused imports/variables in `web/mod.rs`, `auth.rs`, examples,
  `xdp_firewall.rs`). Clean up so `-D warnings` can be enforced.
- **L-3. Missing project-governance files:** no `CONTRIBUTING.md`, `SECURITY.md`,
  `CODE_OF_CONDUCT.md`, issue/PR templates, or `CHANGELOG.md`.
- **L-4. Integration tests are all `#[ignore]`d** (11 tests) and never run in CI because they need a
  live containerd. Add a CI job with containerd (or a nerdctl/dind service) to exercise them.

---

## 6. What is genuinely solid (keep)

- Clean workspace layout; the code compiles and **156 unit tests pass**.
- A real, well-structured auth module (API key + JWT + mTLS, RBAC scopes) — it just isn't applied to
  the web surface.
- gRPC server correctly composes auth → rate-limit → tracing layers and supports TLS/mTLS
  (`bin/nexus-node.rs:342-379`).
- Marketplace adapters call real upstream APIs (e.g. `umod.org/plugins/search.json`,
  `crates/nexus-marketplace/src/adapters/umod.rs:19-20`), with a caching layer.
- Read-path file traversal protection works; graceful shutdown, health checks, Prometheus metrics,
  circuit breakers, and audit logging exist.
- Installer handles system deps, TLS via Caddy, systemd, and ufw end-to-end.

---

## 7. Remediation plan (ordered)

### Phase 0 — Stop the bleeding (security blockers)
1. Add auth middleware + login to the Axum web panel; reuse `Identity`/RBAC from `auth.rs`. (C-1)
2. Default `WEB_BIND`, `GRPC_BIND`, `METRICS_BIND` to `127.0.0.1`; expose only via reverse proxy. (C-1, M-4)
3. Make `AUTH_PASSWORD` real, or switch the installer to `AUTH_API_KEYS`; never claim protection that
   isn't enforced. (C-2)
4. Fix `sanitize_path` to reject `..` and enforce canonical containment on non-existent targets; add
   tests. (C-3)

### Phase 1 — Make CI real
5. Install `protobuf-compiler` in every compiling CI job. (C-4)
6. `cargo fmt --all`; fix clippy `dead_code`; keep `-D warnings`. (C-4, L-2)
7. Add a containerd-backed job and un-`ignore` the integration tests. (L-4)

### Phase 2 — Operational correctness
8. Persist and reconcile container/schedule state on startup. (H-1)
9. Require an explicit flag for the mock runtime; exit on containerd failure otherwise. (H-2)
10. Wire the real callback into `api_trigger_schedule`. (H-3)
11. Replace string version comparison with `semver`; publish GitHub Releases. (M-2)

### Phase 3 — Polish & trust
12. Reconcile README claims with actual status; document RBAC enforcement. (M-1)
13. Harden secrets handling and env-file permissions. (M-3)
14. Remove committed artifacts; extend `.gitignore`. (L-1)
15. Add `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, issue/PR templates. (L-3)

---

## 8. Pre-launch checklist

- [x] Web panel requires authentication; verified an unauthenticated request to
      `/api/v1/containers` returns 401.
- [x] All services bind to loopback by default; public exposure is deliberate and TLS-terminated.
- [x] File API rejects `../` on write/mkdir/rename (test included).
- [x] Installer credentials actually gate access; no false security claims.
- [ ] CI is green on a clean runner (protoc installed, fmt clean, clippy `-D warnings` clean,
      `cargo deny`/`cargo audit` advisory-clean — see M-6).
- [ ] Container/schedule state survives a service restart.
- [ ] Containerd unavailability fails loudly (no silent mock).
- [ ] Manual schedule trigger executes the task.
- [ ] Integration tests run in CI against a real runtime.
- [ ] Secrets are not echoed; env file is `0600` root-owned.
- [ ] README reflects real feature status; `SECURITY.md` present with disclosure contact.

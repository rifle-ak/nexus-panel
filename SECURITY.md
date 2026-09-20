# Security Policy

## Reporting a vulnerability

Please report security issues **privately** — do not open a public issue for a
suspected vulnerability.

- Preferred: open a private security advisory via GitHub
  (**Security → Advisories → Report a vulnerability**) on this repository.
- Include reproduction steps, affected version/commit, and impact.

We aim to acknowledge reports within a few business days and will keep you
updated as we investigate and prepare a fix.

## Supported versions

Nexus Panel is pre-1.0 and under active development. Security fixes are applied
to the `main` branch; there is no long-term-support branch yet.

## Deployment hardening

The node ships secure-by-default, but production operators should verify:

- **Authentication is enabled.** Set `AUTH_ENABLED=true` and provide a
  credential via `AUTH_PASSWORD` or `AUTH_API_KEYS`. With auth enabled but no
  credential configured, the panel **fails closed** (rejects every API request).
  An unauthenticated request to a protected route returns `401`.
- **Bind to loopback unless deliberately exposing.** `WEB_BIND`, `GRPC_BIND`,
  and `METRICS_BIND` all default to `127.0.0.1`. Expose the panel only behind a
  TLS-terminating reverse proxy (the installer can set up Caddy + Let's Encrypt).
- **Protect the config/secrets file.** The installer writes credentials to the
  systemd environment file with mode `0600`, owned by root. Keep it that way,
  rotate credentials by editing the file and restarting the service, and never
  commit secrets to source control.
- **Panel accounts.** Named accounts live in `DATA_DIR/.nexus/users.json`
  (mode 0600) with Argon2id password hashes. Non-admin accounts get
  per-server permission grants and nothing at node level; an admin account
  has the run of the node. The operator password from the environment
  always works as a break-glass credential. Every action is attributed to
  the account, key or customer session that took it.
- **Bucket credentials.** Off-node backup settings, secret key included,
  live in `DATA_DIR/.nexus/remote_backup.json` (mode 0600). The API never
  returns the secret. Give the key a bucket of its own with object read,
  write, list and delete only; the node never needs bucket-level rights.
  Use `https://` endpoints; plain `http://` is accepted only for a bucket
  on the node's own network (MinIO).
- **Billing-system access is an API key.** A WHMCS (or other billing) node
  key in `AUTH_API_KEYS`, or one minted on the Users page (stored as a
  SHA-256 hash, shown once), has full control of the node. Use one key per
  billing system so each can be rotated alone, and serve the panel over HTTPS
  so the key is never on the wire in the clear.
- **Customer sessions are scoped.** A session minted from a billing-portal
  sign-in link can act only on that customer's server; every other route
  answers `403`, including deleting the server, which only the billing
  system's termination does. Sign-in links are single-use, expire after a
  minute, and only their hash is held in memory. The session cookie is
  `HttpOnly`, `SameSite=Lax`, and `Secure` whenever the reverse proxy reports
  `X-Forwarded-Proto: https` — make sure yours does.
- **Logins are throttled.** Ten failed attempts from one address, or two
  hundred node-wide, within fifteen minutes lock the login endpoint for the
  rest of the window (`429` with `Retry-After`). Failures are audited with
  the source address when `AUDIT_ENABLED=true`.
- **Turn on audit logging** (`AUDIT_ENABLED=true`, `AUDIT_LOG_FILE=…`). The
  node records who did what to which server from where: logins, SSO
  sign-ins, every container lifecycle action, and file writes, deletes and
  renames. Entries are hash-chained, so a tampered log shows.
- **Archives and paths are jailed.** Uploads, extractions, downloads and
  every file operation stay inside the server's directory; an archive entry
  that would escape is skipped, not written.
- **Game servers are unprivileged and confined.** Each runs as
  `NEXUS_CONTAINER_UID` (never root) with only the capabilities its
  blueprint grants, `no_new_privileges`, a pids limit, and the standard
  container seccomp allowlist. They do share the host's network namespace,
  so a server can bind any free host port; keep the panel and gRPC ports on
  loopback or behind a firewall. The one-shot install container runs as root
  with host networking, because Pterodactyl-derived install scripts expect
  to — review an imported egg's install script before deploying it.
- **Do not run the mock runtime in production.** The in-memory mock runtime is
  gated behind `NEXUS_DEV_MODE=true` and only pretends to run containers. In
  normal operation a containerd connection failure is fatal by design.

## Scope

Reports about the node/panel code, the gRPC API, the installer, and the
authentication/authorization model are in scope. Findings that require prior
root access to the host, or that target third-party dependencies, may be
redirected upstream.

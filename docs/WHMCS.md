# WHMCS Integration

Nexus ships a WHMCS **provisioning module** — the standard kind of module WHMCS
calls when an order is paid, an invoice goes overdue, a customer upgrades, or a
service is cancelled. It lives in [`whmcs/modules/servers/nexuspanel`](../whmcs/modules/servers/nexuspanel)
and talks to a Nexus node over its REST API.

```
 customer ──orders──▶ WHMCS ──nexuspanel module──▶ Nexus node (REST, API key)
    │                                                     │
    └──"Open game panel" (one-time SSO link)──────────────┘
                 lands in a session scoped to that one server
```

What it does:

| WHMCS event | What happens on the node |
|-------------|--------------------------|
| **Create** (order accepted / paid) | A server is created from the product's blueprint with the product's memory, CPU, disk and variables; free ports are allocated; the game's files are installed; the server starts. The node's server id and the address are stored on the WHMCS service. |
| **Suspend** (overdue invoice) | The server is stopped and marked suspended. Its files stay. |
| **Unsuspend** (payment received) | The suspension is lifted and the server is started again. |
| **Terminate** (cancellation) | The server, its files and its ports are removed. |
| **Change package** (upgrade / downgrade) | New limits and variables are applied and the container rebuilt. The customer's address and files are untouched. |
| **Admin / client buttons** | Start, Stop, Restart; admins also get *Reinstall game files*. |
| **Open game panel** (client area) | WHMCS asks the node for a one-time sign-in link and sends the customer's browser to it. They land on their server's page in a session that can reach **only** that server. |
| **Usage statistics** (daily cron) | Disk used by each server is written to the service's disk usage. |

Creating is idempotent: WHMCS retries a create that timed out, and the node
recognises the service id and returns the server it already made rather than a
second one.

## Requirements

- **WHMCS 8.x** with PHP 8.1 or newer and the `curl` extension (both are WHMCS
  requirements already).
- A **Nexus node** reachable from the WHMCS host over HTTPS, with:
  - authentication enabled and an API key configured (`AUTH_ENABLED=true`,
    `AUTH_API_KEYS=…`);
  - `NODE_PUBLIC_IP` set to the address customers connect to (otherwise the
    IP on WHMCS's server record is shown instead);
  - optionally `PROVISION_PORT_RANGE` (default `20000-29999`).

## 1. Prepare the node

Generate an API key for WHMCS and add it to the node's config:

```bash
openssl rand -hex 32
```

```ini
# /etc/nexus-node/config.env
AUTH_ENABLED=true
AUTH_API_KEYS=<the key you just generated>

# The address customers connect to. The node cannot discover this reliably
# on its own (NAT, DDoS scrubbing), so it is configured.
NODE_PUBLIC_IP=203.0.113.10

# Ports handed to provisioned servers (inclusive). Keep it clear of anything
# else on the host; each server takes one port per port the blueprint declares.
PROVISION_PORT_RANGE=20000-29999
```

Restart the node (`systemctl restart nexus-node`). The key can sit alongside
`AUTH_PASSWORD`; the password is for people, the key is for WHMCS. Several keys
are fine, comma-separated — one per billing system, so any one can be rotated
on its own.

The panel must be served over **HTTPS** for the sign-in link to be safe in
transit; the installer's Caddy setup does this (`ENABLE_TLS=true` with a
domain). If you front the node with a different proxy, make sure it sends
`X-Forwarded-Proto: https`, which is what makes the session cookie `Secure`.

## 2. Install the module

Copy the module directory into your WHMCS installation:

```bash
cp -r whmcs/modules/servers/nexuspanel /path/to/whmcs/modules/servers/
```

There is nothing to run: WHMCS discovers modules by directory. Upgrading is
copying the directory again.

## 3. Add the node as a server in WHMCS

**System Settings → Servers → Add New Server**:

| Field | Value |
|-------|-------|
| Name | Anything — `node1` |
| Hostname | The panel's public hostname, e.g. `panel.example.com` |
| IP Address | The node's game IP (used for the customer's address only when the node has no `NODE_PUBLIC_IP`) |
| Module | **Nexus Panel** |
| Access Hash | The API key from step 1 |
| Secure | **On** |
| Port | `443` (or whatever the panel listens on; `3000` when not behind a proxy) |

Leave Username / Password empty. Click **Test Connection**: it calls the node's
info endpoint with the key and reports what went wrong if anything did.

Put the server in a **server group** and assign products to the group. WHMCS
picks a server from the group per order (round-robin or fill), which is how you
run several nodes: one WHMCS server record per node.

## 4. Create a product

**System Settings → Products/Services → Create a New Product**, then on the
**Module Settings** tab choose **Nexus Panel** and the server group. The
settings:

| Setting | Meaning |
|---------|---------|
| Blueprint | The game. The dropdown is read from the node (`minecraft-paper`, `rust`, `rust-carbon`, `valheim`, `cs2`, `palworld`, `dayz`, plus anything the node ships). |
| Memory (MB) | Hard memory limit for the container. For Java games the blueprint's `MEMORY` variable (the JVM heap) is set to 75 % of this automatically unless you set it yourself. |
| CPU (millicores) | `1000` = one core. Used as both the CPU limit and the relative weight against other servers on the node. |
| Disk (MB) | The allowance shown to the customer and reported as the service's disk limit. |
| Player slots | Sets the blueprint's `MAX_PLAYERS` variable. |
| Variables | One `KEY=VALUE` per line, applied to the blueprint — `SERVER_NAME=Acme Hosting`, `VIEW_DISTANCE=8`, `WORLD_SIZE=3500`. Unknown keys become environment variables. |
| Start after install | Start the server once its game files are installed (default on). |
| Panel session (hours) | How long a customer stays signed in after clicking *Open game panel*. Capped by the node's `WEB_SESSION_TTL_SECS`. |

Set **Module Settings → Automatically setup the product as soon as the first
payment is received**; that is what triggers the create.

### Configurable options and custom fields

Let customers choose sizes with **Configurable Options** on the product. The
module matches option names loosely, ignoring case and punctuation:

| Option name (any of) | Sets |
|----------------------|------|
| `Memory`, `Memory (MB)`, `RAM` | memory in MB (a trailing `MB` is fine) |
| `Memory (GB)`, `RAM (GB)` | memory in GB |
| `CPU`, `CPU (millicores)` | CPU in millicores |
| `Cores`, `CPU Cores`, `vCPU` | CPU in whole cores |
| `Disk`, `Disk (MB)`, `Storage` | disk in MB |
| `Disk (GB)`, `Storage (GB)` | disk in GB |
| `Slots`, `Players`, `Player Slots` | player slots |
| `Blueprint`, `Game` | the blueprint id |

A configurable option overrides the product setting. Changing one on an active
service and running **Change Package** applies it.

**Custom fields** on the product work the same way, plus:

- `Server Name` names the server. Without it the service's domain field is
  used, then `<First name>'s <Game> server`.
- Any field named like an environment variable (`SERVER_PASSWORD`,
  `WORLD_SEED`) sets that blueprint variable. Mark it client-editable to let
  customers fill it in at order time.

## 5. What customers see

The service's **Overview** tab in the client area shows the server's status,
address and ports, resources, and whether its game files are installed, with
Start / Stop / Restart buttons and an **Open game panel** button.

*Open game panel* signs them straight into Nexus. The session:

- can see, start, stop, restart and reinstall **their server only**, use its
  console, files, backups, schedules and the mod marketplace for it;
- cannot see the node, other servers, or the node-level pages (blueprints,
  settings, analytics), and cannot delete the server — only WHMCS's
  termination does that;
- lasts for the product's *Panel session* setting, then they click through
  from WHMCS again. Signing out of the panel ends it early.

The sign-in link itself is valid for **60 seconds and exactly once**; an
expired or reused link shows a page telling them to open the panel from the
billing portal again.

## Usage statistics

WHMCS's daily cron (and **Servers → Update Usage Statistics**) calls the
module once per server. It looks up every active or suspended Nexus service on
that server and records its disk usage and limit, which WHMCS then shows on
the service and can use for overage billing.

## Troubleshooting

Everything the module does is in **Utilities → Logs → Module Log** (enable
logging there first). The node's own error message is what you see: the node
is specific about why it refused something.

| Message | Meaning |
|---------|---------|
| `Authentication required` | The Access Hash is not one of the node's `AUTH_API_KEYS`. |
| `Authentication is enabled but no credential is configured on the node` | `AUTH_ENABLED=true` but neither `AUTH_PASSWORD` nor `AUTH_API_KEYS` is set. |
| `unknown blueprint "…"; this node ships: …` | The product's blueprint is not on this node. |
| `no block of N free ports left in A-B` | The node's `PROVISION_PORT_RANGE` is full. Widen it or add a node to the group. |
| `this blueprint pins port N, which another server on this node already uses` | The blueprint hard-codes a port (DayZ's Steam query port, say) instead of a `{{VARIABLE}}`, so only one such server fits on a node. Use another node or edit the blueprint. |
| `The Nexus node returned a response that is not JSON` | The hostname/port points at something other than the panel (a web server's default page, a login page). |
| `Could not reach the Nexus node at …: SSL certificate problem` | The panel's certificate is not trusted by the WHMCS host. Fix the certificate; the module does not skip verification. |
| `This service has no server provisioned yet.` | The create never succeeded, or the service properties were cleared. Run **Create** on the service again. |

Server ids and addresses are stored as admin-only custom fields (**Server ID**,
**Server IP**, **Server Port**) on the service, visible on its admin page.

## Security notes

- The API key gives full control of the node. It is stored in WHMCS's server
  record (encrypted at rest by WHMCS) and never written to the module log.
- The module verifies the node's TLS certificate and does not follow
  redirects. Turn **Secure** off only for a node on a private network you
  trust end to end.
- Customer sessions are scoped on the node side, not just hidden in the UI:
  the node refuses requests outside the session's server with `403`.
- Sign-in tokens are single-use, expire in a minute, and only their hash is
  held in the node's memory.

## Development

The module has its own test suite that runs on stock PHP with no WHMCS
install:

```bash
php whmcs/tests/run.php
```

It drives the real module functions against a scripted fake node and a fake
WHMCS service model. CI runs it on every push.

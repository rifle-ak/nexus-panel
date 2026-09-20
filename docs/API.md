# gRPC API Reference

Complete reference for the Nexus Node gRPC API.

## Service Definition

All operations are served by a single unified `NodeService`:

```protobuf
service NodeService {
  // Container Lifecycle
  rpc CreateContainer(CreateContainerRequest) returns (CreateContainerResponse);
  rpc StartContainer(StartContainerRequest) returns (StartContainerResponse);
  rpc StopContainer(StopContainerRequest) returns (StopContainerResponse);
  rpc RestartContainer(RestartContainerRequest) returns (RestartContainerResponse);
  rpc DeleteContainer(DeleteContainerRequest) returns (DeleteContainerResponse);
  rpc SuspendContainer(SuspendContainerRequest) returns (SuspendContainerResponse);
  rpc UnsuspendContainer(UnsuspendContainerRequest) returns (UnsuspendContainerResponse);
  rpc ReinstallContainer(ReinstallContainerRequest) returns (ReinstallContainerResponse);

  // Container Queries
  rpc GetContainer(GetContainerRequest) returns (GetContainerResponse);
  rpc ListContainers(ListContainersRequest) returns (ListContainersResponse);

  // Console & Logs
  rpc StreamLogs(StreamLogsRequest) returns (stream LogEntry);
  rpc SendCommand(SendCommandRequest) returns (SendCommandResponse);
  rpc AttachConsole(stream ConsoleInput) returns (stream ConsoleOutput);

  // File Management
  rpc ListFiles(ListFilesRequest) returns (ListFilesResponse);
  rpc ReadFile(ReadFileRequest) returns (ReadFileResponse);
  rpc WriteFile(WriteFileRequest) returns (WriteFileResponse);
  rpc DeleteFile(DeleteFileRequest) returns (DeleteFileResponse);
  rpc RenameFile(RenameFileRequest) returns (RenameFileResponse);
  rpc CopyFile(CopyFileRequest) returns (CopyFileResponse);
  rpc CreateDirectory(CreateDirectoryRequest) returns (CreateDirectoryResponse);
  rpc CompressFiles(CompressFilesRequest) returns (CompressFilesResponse);
  rpc DecompressFile(DecompressFileRequest) returns (DecompressFileResponse);
  rpc DownloadFile(DownloadFileRequest) returns (stream FileChunk);
  rpc UploadFile(stream FileChunk) returns (UploadFileResponse);

  // Backup Management
  rpc CreateBackup(CreateBackupRequest) returns (CreateBackupResponse);
  rpc ListBackups(ListBackupsRequest) returns (ListBackupsResponse);
  rpc RestoreBackup(RestoreBackupRequest) returns (RestoreBackupResponse);
  rpc DeleteBackup(DeleteBackupRequest) returns (DeleteBackupResponse);
  rpc DownloadBackup(DownloadBackupRequest) returns (stream FileChunk);

  // Schedule Management
  rpc CreateSchedule(CreateScheduleRequest) returns (CreateScheduleResponse);
  rpc ListSchedules(ListSchedulesRequest) returns (ListSchedulesResponse);
  rpc UpdateSchedule(UpdateScheduleRequest) returns (UpdateScheduleResponse);
  rpc DeleteSchedule(DeleteScheduleRequest) returns (DeleteScheduleResponse);
  rpc TriggerSchedule(TriggerScheduleRequest) returns (TriggerScheduleResponse);

  // Node Operations
  rpc GetNodeInfo(GetNodeInfoRequest) returns (GetNodeInfoResponse);
  rpc HealthCheck(HealthCheckRequest) returns (HealthCheckResponse);
}
```

## Endpoints

### CreateContainer

Create a new container from a YAML config.

```bash
grpcurl -plaintext -d '{
  "config_yaml": "<yaml content>",
  "container_id": "minecraft-1",
  "auto_start": true
}' localhost:8080 nexus.node.v1.NodeService/CreateContainer
```

Response:
```json
{
  "container_id": "minecraft-1",
  "state": { ... }
}
```

### StartContainer

```bash
grpcurl -plaintext -d '{"container_id": "minecraft-1"}' \
  localhost:8080 nexus.node.v1.NodeService/StartContainer
```

### StopContainer

```bash
grpcurl -plaintext -d '{
  "container_id": "minecraft-1",
  "timeout_secs": 30
}' localhost:8080 nexus.node.v1.NodeService/StopContainer
```

### RestartContainer

```bash
grpcurl -plaintext -d '{"container_id": "minecraft-1"}' \
  localhost:8080 nexus.node.v1.NodeService/RestartContainer
```

### DeleteContainer

```bash
grpcurl -plaintext -d '{
  "container_id": "minecraft-1",
  "force": true
}' localhost:8080 nexus.node.v1.NodeService/DeleteContainer
```

### GetContainer

```bash
grpcurl -plaintext -d '{"container_id": "minecraft-1"}' \
  localhost:8080 nexus.node.v1.NodeService/GetContainer
```

Response:
```json
{
  "container": {
    "id": "minecraft-1",
    "name": "Minecraft Server",
    "state": "RUNNING",
    "image": "itzg/minecraft-server:latest",
    "created_at": "2024-01-15T10:00:00Z",
    "started_at": "2024-01-15T10:00:05Z"
  }
}
```

### ListContainers

```bash
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/ListContainers
```

### StreamLogs

Stream real-time logs from a container.

```bash
grpcurl -plaintext -d '{
  "container_id": "minecraft-1",
  "follow": true,
  "tail": 100
}' localhost:8080 nexus.node.v1.NodeService/StreamLogs
```

### SendCommand

Send a command to the container console.

```bash
grpcurl -plaintext -d '{
  "container_id": "minecraft-1",
  "command": "save-all"
}' localhost:8080 nexus.node.v1.NodeService/SendCommand
```

### GetNodeInfo

```bash
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/GetNodeInfo
```

Response:
```json
{
  "node_id": "prod-node-1",
  "version": "0.1.0",
  "containers_total": 10,
  "containers_running": 8,
  "cpu_cores": 8,
  "memory_total_bytes": 17179869184,
  "memory_available_bytes": 8589934592,
  "disk_total_bytes": 107374182400,
  "disk_available_bytes": 53687091200
}
```

### HealthCheck

```bash
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/HealthCheck
```

## Container States

| State | Description |
|-------|-------------|
| `CREATED` | Container created but not started |
| `RUNNING` | Container is running |
| `STOPPED` | Container stopped normally |
| `PAUSED` | Container is paused |
| `FAILED` | Container crashed or failed |
| `SUSPENDED` | Container is suspended (admin action) |

## Game-File Install States

A server's `install_state` records whether its game files are actually there.
A server cannot be started until they are: its startup command lives in that
directory and does not exist before the install runs.

| State | Description |
|-------|-------------|
| `pending` | The blueprint declares an install that has not run yet |
| `running` | The install is running now |
| `installed` | Game files are in place; the server can start |
| `failed` | The last install failed; the server will not start |
| `not_required` | This blueprint needs no install — its image is self-contained |
| `unknown` | A server created before install state was tracked; reconciled from its directory on the next node restart |

### Install endpoints (REST)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/containers/:id/install` | Start (or re-run) the install. The server must be stopped. 400 if the blueprint declares no install; 409 if one is already running |
| `GET` | `/api/v1/containers/:id/install` | Poll the current/most-recent install job: `status`, `image`, `log`, `exit_code`, `error`, timestamps |

Creating a server starts its install automatically — choosing a game is
choosing to install it — so `POST /api/v1/containers` returns
`{"id": …, "installing": true}` when one was kicked off. With `auto_start`,
the server is started once the install succeeds.

## Node Update Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/v1/node/update-check` | What this node is running and what its channel has available |
| `POST` | `/api/v1/node/update` | Apply an update. 409 if one is running or a server is installing; 501 without systemd |
| `GET` | `/api/v1/node/update` | The current/most-recent update, with its log. Survives the restart the update causes |

`update-check` reports the running build (`version`, `commit`, `commit_date`,
`dirty`), the `channel`, the `latest` available on it, `update_available`, and
`commits_behind` on the `main` channel. When the check cannot reach a
conclusion it sets `error` and leaves `update_available` false — an unreachable
GitHub is not evidence of being up to date.

`POST /node/update` returns as soon as the updater is launched; it cannot
report completion, because completing means restarting the node serving the
request. Poll `GET` for `running`, `succeeded`, `failed`, or `rolled_back`.

## Provisioning Endpoints (REST)

What a billing system uses. All require an admin credential (an API key from
`AUTH_API_KEYS`, as `X-Api-Key` or a bearer token); a customer's scoped
session cannot reach them.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/v1/blueprints` | The blueprints this node ships: `[{id, name, game, version}]` |
| `POST` | `/api/v1/provision/servers` | Create a server for a service. `201` when created, `200` with the existing server when the `external_id` was seen before |
| `GET` | `/api/v1/provision/servers` | List provisioned servers; `?external_id=…` looks one up |
| `GET` | `/api/v1/provision/servers/:id` | One provisioned server with its live status |
| `POST` | `/api/v1/provision/servers/:id/package` | Apply new limits/variables and rebuild the container (ports and files kept). Restarts the server if it was running unless `"restart": false` |
| `DELETE` | `/api/v1/provision/servers/:id` | Terminate: container, files, ports and record. Idempotent: `{"ok": true, "existed": false}` when already gone |
| `GET` | `/api/v1/provision/servers/:id/usage` | `disk_used_bytes`, `disk_limit_bytes`, `memory_limit_bytes`, `status` |
| `POST` | `/api/v1/provision/sso` | Mint a one-time sign-in link for a customer |

Create request:

```json
{
  "external_id": "whmcs-123",
  "name": "Ryan's Minecraft server",
  "blueprint": "minecraft-paper",
  "memory_mb": 4096,
  "cpu_millicores": 2000,
  "disk_mb": 20480,
  "variables": { "MAX_PLAYERS": "20", "SERVER_NAME": "Acme" },
  "port": 25565,
  "auto_start": true,
  "owner": "client-42 <ryan@example.com>"
}
```

`blueprint` names a shipped blueprint; `blueprint_yaml` carries a complete
custom one instead. `memory_mb`, `cpu_millicores` and `disk_mb` become the
blueprint's hard limits (a Java blueprint's `MEMORY` heap variable is derived
from `memory_mb` unless set explicitly). `port` pins the primary port; omitted,
the node allocates from `PROVISION_PORT_RANGE`. Every port the blueprint
declares through a `{{VARIABLE}}` template is allocated; a port the blueprint
pins to a literal number is refused with `409` if another server on the node
already uses it.

Response (`ProvisionedServer`):

```json
{
  "id": "5a6d…", "external_id": "whmcs-123", "name": "…",
  "blueprint": "minecraft-paper", "game": "minecraft",
  "resources": { "memory_mb": 4096, "cpu_millicores": 2000, "disk_mb": 20480 },
  "ports": [ { "name": "game", "port": 20000, "protocol": "tcp", "variable": "SERVER_PORT" },
             { "name": "rcon", "port": 20001, "protocol": "tcp", "variable": "RCON_PORT" } ],
  "primary_port": 20000, "ip": "203.0.113.10",
  "variables": { "MAX_PLAYERS": "20" },
  "status": "stopped", "install_state": "running", "installing": true,
  "created_at": 1758326400, "updated_at": 1758326400
}
```

`ip` is `NODE_PUBLIC_IP`, or `null` when the operator has not set it.

Errors: `400` bad request (unknown blueprint, invalid variable name, memory
below 256 MB…), `409` port conflict, `503` no free ports left in the range.
Creating is serialised on the node, so two concurrent creates for different
services never receive the same port.

### Single sign-on

`POST /api/v1/provision/sso` with `{"server_id": "…", "subject": "client-42",
"ttl_secs": 60, "session_ttl_secs": 28800}` returns
`{"token": "…", "path": "/sso/<token>", "expires_in_secs": 60}`. Send the
customer's browser to `<panel URL><path>`.

`GET /sso/:token` (public) redeems the token **once**: it sets a `nexus_session`
cookie (`HttpOnly`, `SameSite=Lax`, `Secure` behind an HTTPS proxy) holding a
session scoped to that server and redirects to `/#/servers/<id>`. An expired or
reused token gets `401` and a page telling the customer to open the panel from
their billing portal again. `ttl_secs` is capped at 300; `session_ttl_secs` at
the node's `WEB_SESSION_TTL_SECS`.

`GET /api/v1/auth/me` tells a session what it is: `{"scope": "admin"}` or
`{"scope": "servers", "server_ids": ["…"]}`. A scoped session may use
`/api/v1/auth/*`, `GET /api/v1/containers` (filtered to its servers),
everything under `/api/v1/containers/:id/…` for its servers except deleting the
server, and the read-only marketplace routes; everything else answers `403`.

## Observability Endpoints (REST)

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/v1/containers/:id/stats` | `{"running", "current", "history", "interval_secs"}`: the server's latest resource sample and the last ten minutes of them, oldest first |
| `GET` | `/api/v1/containers/:id/console?bytes=65536` | `{"text"}`: the tail of the console log (up to 512 KiB) |
| `GET` | `/api/v1/containers/:id/console/stream` | Server-sent events, one `line` event per line of new output, keep-alives every 15 s. Starts at the current end of the log; fetch history first |
| `GET` | `/api/v1/node/stats` | Operator only. Node CPU %, memory, swap, load averages, with history, and every running server's latest sample |
| `GET` | `/api/v1/node/health` | Operator only. Checks: `containerd` (a version round-trip), `disk` (free space on the data directory's filesystem against `MIN_DISK_SPACE_BYTES`), `memory` (available against `MIN_MEMORY_BYTES`), `data_directory` (writable), `firewall` (on), `servers` (no crash loops). Each is `pass`, `warn` or `fail` |

A resource sample has `cpu_percent` (of one core; 200 means two cores),
`memory_bytes` (with page cache), `memory_anon_bytes` (what the process
holds), `memory_limit_bytes` (absent when unlimited), `pids`, cumulative
`io_read_bytes`/`io_write_bytes` and their rates `io_read_bps`/`io_write_bps`.
Samples are taken every 5 seconds from the container's cgroup; the same
numbers feed `nexus_node_container_cpu_usage_millicores` and
`nexus_node_container_memory_usage_bytes` on `/metrics`. `GET
/api/v1/containers` and `GET /api/v1/containers/:id` carry the latest
sample as `usage` while the server runs.

Network traffic is not attributed per server: servers share the host's
network namespace, so the kernel keeps no per-container counters.

## Firewall Endpoints (REST)

Operator (admin session or API key):

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/v1/firewall` | Node-wide status: backend, protections with counters, blocklist, trusted list, attached servers |
| `POST` | `/api/v1/firewall/blocks` | `{"cidr": "198.51.100.0/24", "ttl_secs": 3600, "reason": "…"}`; omit `ttl_secs` for a permanent block |
| `POST` | `/api/v1/firewall/unblock` | `{"cidr": "…"}` |
| `POST` | `/api/v1/firewall/trusted` | `{"cidr": "…"}`: never filtered |
| `POST` | `/api/v1/firewall/untrust` | `{"cidr": "…"}` |

Per server (operator, or the customer session that owns it):

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/v1/containers/:id/firewall` | `{"enabled", "rules", "applied"}`: the blueprint's rules, and what is in the kernel with counters while the server runs |
| `PUT` | `/api/v1/containers/:id/firewall` | `{"rules": [...]}` replaces the rules (stored in the blueprint; applied at once if running). At most 200 |
| `POST` | `/api/v1/containers/:id/firewall/blocks` | `{"cidr": "…", "name": "…"}` adds a `block_cidr` rule: the one-click ban |

Invalid CIDRs and rates answer `400`. When the node's firewall is off,
rules are still stored and `enabled` is `false`.

## Error Codes

| Code | Status | Description |
|------|--------|-------------|
| 0 | OK | Success |
| 3 | INVALID_ARGUMENT | Bad request (malformed YAML, etc.) |
| 5 | NOT_FOUND | Container not found |
| 6 | ALREADY_EXISTS | Container already exists |
| 7 | PERMISSION_DENIED | Authentication failed |
| 8 | RESOURCE_EXHAUSTED | Rate limit exceeded |
| 13 | INTERNAL | Internal error |
| 14 | UNAVAILABLE | Service unavailable |

## Authentication

### API Key

```bash
grpcurl -H "x-api-key: <api-key>" \
  -plaintext localhost:8080 nexus.node.v1.NodeService/ListContainers
```

Or via Bearer token:

```bash
grpcurl -H "authorization: Bearer <jwt-token>" \
  -plaintext localhost:8080 nexus.node.v1.NodeService/ListContainers
```

### mTLS

```bash
grpcurl \
  -cacert ca.crt \
  -cert client.crt \
  -key client.key \
  localhost:8080 nexus.node.v1.NodeService/ListContainers
```

## Rate Limiting

Default limits:
- 10,000 requests/second global
- 100 requests/second per client
- 20 concurrent requests per client

Headers returned:
- `x-ratelimit-limit`: Request limit
- `x-ratelimit-remaining`: Remaining requests
- `x-ratelimit-reset`: Reset timestamp

## File, Backup, and Schedule Endpoints

All file management, backup, and schedule RPCs are part of the unified `NodeService`
(see the full service definition above). There are no separate `FileService`,
`BackupService`, or `ScheduleService` services.

## Performance

- Request latency: <10ms (p99)
- Throughput: 1000+ requests/sec
- Concurrent connections: 1000+
- Streaming: Backpressure-aware

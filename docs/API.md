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

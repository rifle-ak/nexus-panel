# gRPC API Reference

Complete reference for the Nexus Node gRPC API.

## Service Definition

```protobuf
service NodeService {
  // Container Lifecycle
  rpc CreateContainer(CreateContainerRequest) returns (CreateContainerResponse);
  rpc StartContainer(StartContainerRequest) returns (StartContainerResponse);
  rpc StopContainer(StopContainerRequest) returns (StopContainerResponse);
  rpc RestartContainer(RestartContainerRequest) returns (RestartContainerResponse);
  rpc DeleteContainer(DeleteContainerRequest) returns (DeleteContainerResponse);

  // Container Queries
  rpc GetContainer(GetContainerRequest) returns (GetContainerResponse);
  rpc ListContainers(ListContainersRequest) returns (ListContainersResponse);

  // Console & Logs
  rpc StreamLogs(StreamLogsRequest) returns (stream LogEntry);
  rpc SendCommand(SendCommandRequest) returns (SendCommandResponse);
  rpc AttachConsole(stream ConsoleInput) returns (stream ConsoleOutput);

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
  "success": true
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
| `FAILED` | Container crashed or failed |
| `RESTARTING` | Container is restarting |

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
grpcurl -H "authorization: Bearer <api-key>" \
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
- 1000 requests/minute per client
- 100 concurrent connections

Headers returned:
- `x-ratelimit-limit`: Request limit
- `x-ratelimit-remaining`: Remaining requests
- `x-ratelimit-reset`: Reset timestamp

## File Management Endpoints

```protobuf
service FileService {
  rpc ListFiles(ListFilesRequest) returns (ListFilesResponse);
  rpc ReadFile(ReadFileRequest) returns (ReadFileResponse);
  rpc WriteFile(WriteFileRequest) returns (WriteFileResponse);
  rpc DeleteFiles(DeleteFilesRequest) returns (DeleteFilesResponse);
  rpc Rename(RenameRequest) returns (RenameResponse);
  rpc Copy(CopyRequest) returns (CopyResponse);
  rpc CreateDirectory(CreateDirectoryRequest) returns (CreateDirectoryResponse);
  rpc Compress(CompressRequest) returns (CompressResponse);
  rpc Decompress(DecompressRequest) returns (DecompressResponse);
}
```

## Backup Endpoints

```protobuf
service BackupService {
  rpc CreateBackup(CreateBackupRequest) returns (CreateBackupResponse);
  rpc ListBackups(ListBackupsRequest) returns (ListBackupsResponse);
  rpc GetBackup(GetBackupRequest) returns (GetBackupResponse);
  rpc RestoreBackup(RestoreBackupRequest) returns (RestoreBackupResponse);
  rpc DeleteBackup(DeleteBackupRequest) returns (DeleteBackupResponse);
}
```

## Schedule Endpoints

```protobuf
service ScheduleService {
  rpc CreateSchedule(CreateScheduleRequest) returns (CreateScheduleResponse);
  rpc ListSchedules(ListSchedulesRequest) returns (ListSchedulesResponse);
  rpc UpdateSchedule(UpdateScheduleRequest) returns (UpdateScheduleResponse);
  rpc DeleteSchedule(DeleteScheduleRequest) returns (DeleteScheduleResponse);
  rpc TriggerSchedule(TriggerScheduleRequest) returns (TriggerScheduleResponse);
}
```

## Marketplace Endpoints

```protobuf
service MarketplaceService {
  rpc SearchMods(SearchModsRequest) returns (SearchModsResponse);
  rpc GetMod(GetModRequest) returns (GetModResponse);
  rpc DownloadMod(DownloadModRequest) returns (DownloadModResponse);
  rpc CheckUpdates(CheckUpdatesRequest) returns (CheckUpdatesResponse);
}
```

## Performance

- Request latency: <10ms (p99)
- Throughput: 1000+ requests/sec
- Concurrent connections: 1000+
- Streaming: Backpressure-aware

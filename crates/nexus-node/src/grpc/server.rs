use super::proto::{
    node_service_server::{NodeService, NodeServiceServer},
    *,
};
use crate::container::ContainerManager;
use crate::health::HealthChecker;
use crate::metrics::Metrics;
use nexus_config::GameConfig;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use tracing::info;

/// gRPC server implementation for Nexus Node
pub struct NodeServiceImpl {
    manager: Arc<ContainerManager>,
    node_id: String,
    start_time: SystemTime,
    metrics: Arc<Metrics>,
    health_checker: Arc<RwLock<HealthChecker>>,
}

impl NodeServiceImpl {
    /// Create a new NodeService
    pub fn new(
        manager: Arc<ContainerManager>,
        node_id: String,
        health_checker: Arc<RwLock<HealthChecker>>,
    ) -> Self {
        let metrics = manager.metrics().clone();
        Self {
            manager,
            node_id,
            start_time: SystemTime::now(),
            metrics,
            health_checker,
        }
    }

    /// Create a gRPC server instance
    pub fn into_server(self) -> NodeServiceServer<Self> {
        NodeServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl NodeService for NodeServiceImpl {
    async fn create_container(
        &self,
        request: Request<CreateContainerRequest>,
    ) -> std::result::Result<Response<CreateContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: CreateContainer request");

        // Parse GameConfig from YAML
        let config = GameConfig::from_yaml(&req.config_yaml)
            .map_err(|e| {
                self.metrics.record_grpc_request("CreateContainer", "invalid_argument", start.elapsed());
                Status::invalid_argument(format!("Invalid config: {}", e))
            })?;

        // Create container
        let container_id = self
            .manager
            .create_container(&config, req.container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("CreateContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to create container: {}", e))
            })?;

        // Auto-start if requested
        if req.auto_start {
            self.manager
                .start_container(&container_id)
                .await
                .map_err(|e| {
                    self.metrics.record_grpc_request("CreateContainer", "internal_error", start.elapsed());
                    Status::internal(format!("Failed to start container: {}", e))
                })?;
        }

        // Get container state
        let state = self
            .manager
            .get_state(&container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("CreateContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to get state: {}", e))
            })?;

        self.metrics.record_grpc_request("CreateContainer", "ok", start.elapsed());

        Ok(Response::new(CreateContainerResponse {
            container_id,
            state: Some(convert_container_state(&state)),
        }))
    }

    async fn start_container(
        &self,
        request: Request<StartContainerRequest>,
    ) -> std::result::Result<Response<StartContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: StartContainer {}", req.container_id);

        // Start container
        self.manager
            .start_container(&req.container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("StartContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to start container: {}", e))
            })?;

        // Get updated state
        let state = self
            .manager
            .get_state(&req.container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("StartContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to get state: {}", e))
            })?;

        let pid = state.pid.unwrap_or(0);

        self.metrics.record_grpc_request("StartContainer", "ok", start.elapsed());

        Ok(Response::new(StartContainerResponse {
            pid,
            state: Some(convert_container_state(&state)),
        }))
    }

    async fn stop_container(
        &self,
        request: Request<StopContainerRequest>,
    ) -> std::result::Result<Response<StopContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: StopContainer {}", req.container_id);

        let timeout = req.timeout_secs.or(Some(30));

        // Stop container
        self.manager
            .stop_container(&req.container_id, timeout)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("StopContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to stop container: {}", e))
            })?;

        // Get updated state
        let state = self
            .manager
            .get_state(&req.container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("StopContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to get state: {}", e))
            })?;

        let exit_code = state.exit_code.unwrap_or(0);

        self.metrics.record_grpc_request("StopContainer", "ok", start.elapsed());

        Ok(Response::new(StopContainerResponse {
            exit_code,
            state: Some(convert_container_state(&state)),
        }))
    }

    async fn restart_container(
        &self,
        request: Request<RestartContainerRequest>,
    ) -> std::result::Result<Response<RestartContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: RestartContainer {}", req.container_id);

        // Restart container
        self.manager
            .restart_container(&req.container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("RestartContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to restart container: {}", e))
            })?;

        // Get updated state
        let state = self
            .manager
            .get_state(&req.container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("RestartContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to get state: {}", e))
            })?;

        let pid = state.pid.unwrap_or(0);

        self.metrics.record_grpc_request("RestartContainer", "ok", start.elapsed());

        Ok(Response::new(RestartContainerResponse {
            pid,
            state: Some(convert_container_state(&state)),
        }))
    }

    async fn delete_container(
        &self,
        request: Request<DeleteContainerRequest>,
    ) -> std::result::Result<Response<DeleteContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: DeleteContainer {}", req.container_id);

        // Delete container
        self.manager
            .delete_container(&req.container_id, req.force)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("DeleteContainer", "internal_error", start.elapsed());
                Status::internal(format!("Failed to delete container: {}", e))
            })?;

        self.metrics.record_grpc_request("DeleteContainer", "ok", start.elapsed());

        Ok(Response::new(DeleteContainerResponse { success: true }))
    }

    async fn get_container(
        &self,
        request: Request<GetContainerRequest>,
    ) -> std::result::Result<Response<GetContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: GetContainer {}", req.container_id);

        // Get container state
        let state = self
            .manager
            .get_state(&req.container_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("GetContainer", "not_found", start.elapsed());
                Status::not_found(format!("Container not found: {}", e))
            })?;

        self.metrics.record_grpc_request("GetContainer", "ok", start.elapsed());

        Ok(Response::new(GetContainerResponse {
            state: Some(convert_container_state(&state)),
        }))
    }

    async fn list_containers(
        &self,
        _request: Request<ListContainersRequest>,
    ) -> std::result::Result<Response<ListContainersResponse>, Status> {
        let start = Instant::now();
        info!("gRPC: ListContainers");

        // List all containers
        let containers = self.manager.list_containers().await;

        let states = containers
            .iter()
            .map(convert_container_state)
            .collect();

        self.metrics.record_grpc_request("ListContainers", "ok", start.elapsed());

        Ok(Response::new(ListContainersResponse {
            containers: states,
        }))
    }

    async fn stream_logs(
        &self,
        request: Request<StreamLogsRequest>,
    ) -> std::result::Result<Response<Self::StreamLogsStream>, Status> {
        let req = request.into_inner();
        let container_id = req.container_id.clone();

        info!("gRPC: StreamLogs {} (follow: {})", container_id, req.follow);

        // Attach to container console
        let mut console = self
            .manager
            .attach_console(&container_id)
            .await
            .map_err(|e| Status::not_found(format!("Container not found: {}", e)))?;

        // Create a channel for streaming log entries
        let (tx, rx) = tokio::sync::mpsc::channel(128);

        // Spawn a task to read from console and send to channel
        tokio::spawn(async move {
            let mut line_count = 0;
            let tail = req.tail as usize;
            let follow = req.follow;
            let mut buffer = Vec::new();

            loop {
                match console.read_line().await {
                    Ok(Some(line)) => {
                        line_count += 1;

                        // If tailing, buffer the lines
                        if tail > 0 && !follow {
                            buffer.push(line.clone());
                            if buffer.len() > tail {
                                buffer.remove(0);
                            }
                            continue;
                        }

                        // Create log entry
                        let entry = LogEntry {
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            stream: "stdout".to_string(),
                            line,
                        };

                        // Send to client
                        if tx.send(Ok(entry)).await.is_err() {
                            // Client disconnected
                            break;
                        }
                    }
                    Ok(None) => {
                        // EOF reached
                        if tail > 0 && !follow {
                            // Send buffered tail lines
                            for line in buffer {
                                let entry = LogEntry {
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                    stream: "stdout".to_string(),
                                    line,
                                };
                                if tx.send(Ok(entry)).await.is_err() {
                                    break;
                                }
                            }
                        }
                        break;
                    }
                    Err(e) => {
                        // Error reading logs
                        let _ = tx
                            .send(Err(Status::internal(format!("Error reading logs: {}", e))))
                            .await;
                        break;
                    }
                }
            }

            info!("StreamLogs for {} completed ({} lines)", container_id, line_count);
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    type StreamLogsStream = tokio_stream::wrappers::ReceiverStream<std::result::Result<LogEntry, Status>>;

    async fn send_command(
        &self,
        request: Request<SendCommandRequest>,
    ) -> std::result::Result<Response<SendCommandResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: SendCommand {} -> {}", req.container_id, req.command);

        // Send command to container
        match self.manager.send_command(&req.container_id, &req.command).await {
            Ok(_) => {
                self.metrics.record_grpc_request("SendCommand", "ok", start.elapsed());
                Ok(Response::new(SendCommandResponse {
                    success: true,
                    error: None,
                }))
            }
            Err(e) => {
                self.metrics.record_grpc_request("SendCommand", "error", start.elapsed());
                Ok(Response::new(SendCommandResponse {
                    success: false,
                    error: Some(format!("{}", e)),
                }))
            }
        }
    }

    type AttachConsoleStream = tokio_stream::wrappers::ReceiverStream<std::result::Result<ConsoleOutput, Status>>;

    async fn attach_console(
        &self,
        request: Request<tonic::Streaming<ConsoleInput>>,
    ) -> std::result::Result<Response<Self::AttachConsoleStream>, Status> {
        let mut input_stream = request.into_inner();

        // Wait for the first message to get container_id
        let first_msg = input_stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("Failed to read first message: {}", e)))?
            .ok_or_else(|| Status::invalid_argument("No input message received"))?;

        let container_id = first_msg.container_id.clone();
        info!("gRPC: AttachConsole {}", container_id);

        // Attach to container console
        let mut console = self
            .manager
            .attach_bidirectional(&container_id)
            .await
            .map_err(|e| Status::not_found(format!("Failed to attach: {}", e)))?;

        // Process the first message's input if any
        if let Some(input) = first_msg.input {
            match input {
                console_input::Input::Data(data) => {
                    if let Err(e) = console.write(data.as_bytes()).await {
                        return Err(Status::internal(format!("Write failed: {}", e)));
                    }
                }
                console_input::Input::Resize(_) => {
                    let rows = first_msg.rows.unwrap_or(24) as u16;
                    let cols = first_msg.cols.unwrap_or(80) as u16;
                    let _ = console.resize(rows, cols).await;
                }
            }
        }

        // Create output channel
        let (tx, rx) = tokio::sync::mpsc::channel(128);

        // Spawn task to handle bidirectional streaming
        let container_id_clone = container_id.clone();
        tokio::spawn(async move {
            // Spawn reader task
            let tx_reader = tx.clone();
            let reader_handle = tokio::spawn(async move {
                loop {
                    match console.read().await {
                        Ok(Some(data)) => {
                            let output = ConsoleOutput {
                                output: Some(console_output::Output::Data(
                                    String::from_utf8_lossy(&data).to_string()
                                )),
                                error: None,
                            };
                            if tx_reader.send(Ok(output)).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => {
                            // No data available, small delay
                            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                        }
                        Err(e) => {
                            let output = ConsoleOutput {
                                output: None,
                                error: Some(format!("{}", e)),
                            };
                            let _ = tx_reader.send(Ok(output)).await;
                            break;
                        }
                    }

                    if !console.is_open() {
                        break;
                    }
                }

                // Send closed signal
                let _ = tx_reader.send(Ok(ConsoleOutput {
                    output: Some(console_output::Output::Closed(true)),
                    error: None,
                })).await;
            });

            // Handle incoming input messages
            while let Ok(Some(msg)) = input_stream.message().await {
                if let Some(input) = msg.input {
                    match input {
                        console_input::Input::Data(data) => {
                            // We can't easily write here since console is moved
                            // In a real implementation, we'd use a channel or Arc<Mutex>
                            info!("Console input for {}: {} bytes", msg.container_id, data.len());
                        }
                        console_input::Input::Resize(_) => {
                            let rows = msg.rows.unwrap_or(24) as u16;
                            let cols = msg.cols.unwrap_or(80) as u16;
                            info!("Console resize for {}: {}x{}", msg.container_id, cols, rows);
                        }
                    }
                }
            }

            // Wait for reader to finish
            let _ = reader_handle.await;
            info!("AttachConsole for {} completed", container_id_clone);
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn get_node_info(
        &self,
        _request: Request<GetNodeInfoRequest>,
    ) -> std::result::Result<Response<GetNodeInfoResponse>, Status> {
        let start = Instant::now();
        info!("gRPC: GetNodeInfo");

        let uptime = self
            .start_time
            .elapsed()
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Update uptime metric
        self.metrics.update_uptime(uptime as u64);

        let container_count = self.manager.list_containers().await.len() as u32;

        self.metrics.record_grpc_request("GetNodeInfo", "ok", start.elapsed());

        Ok(Response::new(GetNodeInfoResponse {
            node_id: self.node_id.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            resources: Some(NodeResources {
                // TODO: Get actual resource info from system
                total_cpu_millicores: 4000,
                total_memory_bytes: 8 * 1024 * 1024 * 1024,
                total_disk_bytes: 100 * 1024 * 1024 * 1024,
                used_cpu_millicores: 0,
                used_memory_bytes: 0,
                used_disk_bytes: 0,
            }),
            container_count,
            uptime_secs: uptime,
        }))
    }

    async fn health_check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> std::result::Result<Response<HealthCheckResponse>, Status> {
        let start = Instant::now();
        info!("gRPC: HealthCheck");

        // Run comprehensive health checks
        let mut checker = self.health_checker.write().await;
        let health_result = checker.check().await;

        let status = match health_result.status {
            crate::health::HealthStatus::Healthy => HealthStatus::Healthy,
            crate::health::HealthStatus::Degraded => HealthStatus::Degraded,
            crate::health::HealthStatus::Unhealthy => HealthStatus::Unhealthy,
        };

        let mut checks = std::collections::HashMap::new();
        for (component, component_health) in &health_result.checks {
            checks.insert(
                component.clone(),
                format!("{:?}", component_health.status),
            );
        }

        let message = health_result.message.unwrap_or_else(|| {
            format!("Health check completed: {}", health_result.status.as_str())
        });

        self.metrics.record_grpc_request("HealthCheck", "ok", start.elapsed());

        Ok(Response::new(HealthCheckResponse {
            status: status as i32,
            message,
            checks,
        }))
    }
}

/// Convert internal ContainerState to protobuf ContainerState
fn convert_container_state(state: &crate::container::ContainerState) -> ContainerState {
    use crate::container::state::ContainerStatus as InternalStatus;

    let status = match state.status {
        InternalStatus::Created => ContainerStatus::StatusCreated,
        InternalStatus::Running => ContainerStatus::StatusRunning,
        InternalStatus::Paused => ContainerStatus::StatusRunning, // Treat paused as running for now
        InternalStatus::Stopped => ContainerStatus::StatusStopped,
        InternalStatus::Failed => ContainerStatus::StatusFailed,
    };

    ContainerState {
        id: state.id.clone(),
        name: state.name.clone(),
        status: status as i32,
        pid: state.pid,
        exit_code: state.exit_code,
        image: state.image.clone(),
        created_at: state
            .created_at
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        started_at: state
            .started_at
            .map(|t| t.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64),
        stopped_at: state
            .stopped_at
            .map(|t| t.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64),
        restart_count: state.restart_count,
    }
}

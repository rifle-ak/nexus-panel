use super::proto::{
    node_service_server::{NodeService, NodeServiceServer},
    *,
};
use crate::backup::BackupManager;
use crate::circuit_breaker::CircuitBreakerRegistry;
use crate::container::ContainerManager;
use crate::files::FileManager;
use crate::health::HealthChecker;
use crate::metrics::Metrics;
use crate::schedule::{
    ScheduleManager, ScheduleTask as InternalScheduleTask,
    ScheduleTaskType as InternalScheduleTaskType,
};
use crate::validation::Validator;
use nexus_config::GameConfig;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use tracing::info;

/// Chunk size for streaming file operations (64KB)
const STREAM_CHUNK_SIZE: usize = 64 * 1024;

/// gRPC server implementation for Nexus Node
pub struct NodeServiceImpl {
    manager: Arc<ContainerManager>,
    node_id: String,
    start_time: SystemTime,
    metrics: Arc<Metrics>,
    health_checker: Arc<RwLock<HealthChecker>>,
    backup_manager: Arc<BackupManager>,
    schedule_manager: Arc<ScheduleManager>,
    circuit_breakers: Arc<CircuitBreakerRegistry>,
    validator: Arc<Validator>,
}

impl NodeServiceImpl {
    /// Create a new NodeService
    pub fn new(
        manager: Arc<ContainerManager>,
        node_id: String,
        health_checker: Arc<RwLock<HealthChecker>>,
        backup_manager: Arc<BackupManager>,
        schedule_manager: Arc<ScheduleManager>,
    ) -> Self {
        let metrics = manager.metrics().clone();
        Self {
            manager,
            node_id,
            start_time: SystemTime::now(),
            metrics,
            health_checker,
            backup_manager,
            schedule_manager,
            circuit_breakers: Arc::new(CircuitBreakerRegistry::new(Default::default())),
            validator: Arc::new(Validator::default()),
        }
    }

    /// Create a new NodeService with enterprise components
    pub fn with_enterprise(
        manager: Arc<ContainerManager>,
        node_id: String,
        health_checker: Arc<RwLock<HealthChecker>>,
        backup_manager: Arc<BackupManager>,
        schedule_manager: Arc<ScheduleManager>,
        circuit_breakers: Arc<CircuitBreakerRegistry>,
        validator: Arc<Validator>,
    ) -> Self {
        let metrics = manager.metrics().clone();
        Self {
            manager,
            node_id,
            start_time: SystemTime::now(),
            metrics,
            health_checker,
            backup_manager,
            schedule_manager,
            circuit_breakers,
            validator,
        }
    }

    /// Create a gRPC server instance
    pub fn into_server(self) -> NodeServiceServer<Self> {
        NodeServiceServer::new(self)
    }

    /// Create a FileManager for a container
    fn file_manager(&self, container_id: &str) -> FileManager {
        FileManager::new(container_id, self.manager.data_dir())
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

        // Validate input
        if let Some(ref id) = req.container_id {
            self.validator.validate_container_id(id).map_err(|e| {
                self.metrics.record_grpc_request(
                    "CreateContainer",
                    "invalid_argument",
                    start.elapsed(),
                );
                Status::invalid_argument(format!("Invalid container ID: {}", e))
            })?;
        }

        self.validator.validate_config_yaml(&req.config_yaml).map_err(|e| {
            self.metrics.record_grpc_request(
                "CreateContainer",
                "invalid_argument",
                start.elapsed(),
            );
            Status::invalid_argument(format!("Invalid config: {}", e))
        })?;

        // Check circuit breaker for containerd operations
        let breaker = self.circuit_breakers.get("containerd");
        if !breaker.allows_request() {
            self.metrics
                .record_grpc_request("CreateContainer", "unavailable", start.elapsed());
            return Err(Status::unavailable(
                "Container runtime temporarily unavailable",
            ));
        }

        // Parse GameConfig from YAML
        let config = GameConfig::from_yaml(&req.config_yaml).map_err(|e| {
            self.metrics.record_grpc_request(
                "CreateContainer",
                "invalid_argument",
                start.elapsed(),
            );
            Status::invalid_argument(format!("Invalid config: {}", e))
        })?;

        // Create container
        let container_id =
            self.manager.create_container(&config, req.container_id).await.map_err(|e| {
                breaker.record_failure(&e.to_string());
                self.metrics.record_grpc_request(
                    "CreateContainer",
                    "internal_error",
                    start.elapsed(),
                );
                Status::internal(format!("Failed to create container: {}", e))
            })?;

        breaker.record_success();

        // Auto-start if requested
        if req.auto_start {
            self.manager.start_container(&container_id).await.map_err(|e| {
                self.metrics.record_grpc_request(
                    "CreateContainer",
                    "internal_error",
                    start.elapsed(),
                );
                Status::internal(format!("Failed to start container: {}", e))
            })?;
        }

        // Get container state
        let state = self.manager.get_state(&container_id).await.map_err(|e| {
            self.metrics
                .record_grpc_request("CreateContainer", "internal_error", start.elapsed());
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

        // Validate input
        self.validator.validate_container_id(&req.container_id).map_err(|e| {
            self.metrics
                .record_grpc_request("StartContainer", "invalid_argument", start.elapsed());
            Status::invalid_argument(format!("Invalid container ID: {}", e))
        })?;

        // Check circuit breaker
        let breaker = self.circuit_breakers.get("containerd");
        if !breaker.allows_request() {
            self.metrics
                .record_grpc_request("StartContainer", "unavailable", start.elapsed());
            return Err(Status::unavailable(
                "Container runtime temporarily unavailable",
            ));
        }

        // Start container
        self.manager.start_container(&req.container_id).await.map_err(|e| {
            breaker.record_failure(&e.to_string());
            self.metrics
                .record_grpc_request("StartContainer", "internal_error", start.elapsed());
            Status::internal(format!("Failed to start container: {}", e))
        })?;

        breaker.record_success();

        // Get updated state
        let state = self.manager.get_state(&req.container_id).await.map_err(|e| {
            self.metrics
                .record_grpc_request("StartContainer", "internal_error", start.elapsed());
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

        // Validate input
        self.validator.validate_container_id(&req.container_id).map_err(|e| {
            self.metrics
                .record_grpc_request("StopContainer", "invalid_argument", start.elapsed());
            Status::invalid_argument(format!("Invalid container ID: {}", e))
        })?;

        let timeout = req.timeout_secs.or(Some(30));

        // Check circuit breaker
        let breaker = self.circuit_breakers.get("containerd");
        if !breaker.allows_request() {
            self.metrics
                .record_grpc_request("StopContainer", "unavailable", start.elapsed());
            return Err(Status::unavailable(
                "Container runtime temporarily unavailable",
            ));
        }

        // Stop container
        self.manager.stop_container(&req.container_id, timeout).await.map_err(|e| {
            breaker.record_failure(&e.to_string());
            self.metrics
                .record_grpc_request("StopContainer", "internal_error", start.elapsed());
            Status::internal(format!("Failed to stop container: {}", e))
        })?;

        breaker.record_success();

        // Get updated state
        let state = self.manager.get_state(&req.container_id).await.map_err(|e| {
            self.metrics
                .record_grpc_request("StopContainer", "internal_error", start.elapsed());
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
        self.manager.restart_container(&req.container_id).await.map_err(|e| {
            self.metrics
                .record_grpc_request("RestartContainer", "internal_error", start.elapsed());
            Status::internal(format!("Failed to restart container: {}", e))
        })?;

        // Get updated state
        let state = self.manager.get_state(&req.container_id).await.map_err(|e| {
            self.metrics
                .record_grpc_request("RestartContainer", "internal_error", start.elapsed());
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
        self.manager.delete_container(&req.container_id, req.force).await.map_err(|e| {
            self.metrics
                .record_grpc_request("DeleteContainer", "internal_error", start.elapsed());
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
        let state = self.manager.get_state(&req.container_id).await.map_err(|e| {
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

        let states = containers.iter().map(convert_container_state).collect();

        self.metrics.record_grpc_request("ListContainers", "ok", start.elapsed());

        Ok(Response::new(ListContainersResponse { containers: states }))
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

            info!(
                "StreamLogs for {} completed ({} lines)",
                container_id, line_count
            );
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    type StreamLogsStream =
        tokio_stream::wrappers::ReceiverStream<std::result::Result<LogEntry, Status>>;

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

    type AttachConsoleStream =
        tokio_stream::wrappers::ReceiverStream<std::result::Result<ConsoleOutput, Status>>;

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
                                    String::from_utf8_lossy(&data).to_string(),
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
                let _ = tx_reader
                    .send(Ok(ConsoleOutput {
                        output: Some(console_output::Output::Closed(true)),
                        error: None,
                    }))
                    .await;
            });

            // Handle incoming input messages
            while let Ok(Some(msg)) = input_stream.message().await {
                if let Some(input) = msg.input {
                    match input {
                        console_input::Input::Data(data) => {
                            // We can't easily write here since console is moved
                            // In a real implementation, we'd use a channel or Arc<Mutex>
                            info!(
                                "Console input for {}: {} bytes",
                                msg.container_id,
                                data.len()
                            );
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

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    async fn get_node_info(
        &self,
        _request: Request<GetNodeInfoRequest>,
    ) -> std::result::Result<Response<GetNodeInfoResponse>, Status> {
        let start = Instant::now();
        info!("gRPC: GetNodeInfo");

        let uptime = self.start_time.elapsed().map(|d| d.as_secs() as i64).unwrap_or(0);

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
            checks.insert(component.clone(), format!("{:?}", component_health.status));
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

    async fn suspend_container(
        &self,
        request: Request<SuspendContainerRequest>,
    ) -> std::result::Result<Response<SuspendContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: SuspendContainer {}", req.container_id);

        self.manager.suspend_container(&req.container_id).await.map_err(|e| {
            self.metrics
                .record_grpc_request("SuspendContainer", "internal_error", start.elapsed());
            Status::internal(format!("Failed to suspend container: {}", e))
        })?;

        let state = self
            .manager
            .get_state(&req.container_id)
            .await
            .map_err(|e| Status::internal(format!("Failed to get state: {}", e)))?;

        self.metrics.record_grpc_request("SuspendContainer", "ok", start.elapsed());

        Ok(Response::new(SuspendContainerResponse {
            success: true,
            state: Some(convert_container_state(&state)),
        }))
    }

    async fn unsuspend_container(
        &self,
        request: Request<UnsuspendContainerRequest>,
    ) -> std::result::Result<Response<UnsuspendContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: UnsuspendContainer {}", req.container_id);

        self.manager.unsuspend_container(&req.container_id).await.map_err(|e| {
            self.metrics.record_grpc_request(
                "UnsuspendContainer",
                "internal_error",
                start.elapsed(),
            );
            Status::internal(format!("Failed to unsuspend container: {}", e))
        })?;

        // Auto-start if requested
        if req.auto_start {
            self.manager.start_container(&req.container_id).await.map_err(|e| {
                self.metrics.record_grpc_request(
                    "UnsuspendContainer",
                    "internal_error",
                    start.elapsed(),
                );
                Status::internal(format!("Failed to start container: {}", e))
            })?;
        }

        let state = self
            .manager
            .get_state(&req.container_id)
            .await
            .map_err(|e| Status::internal(format!("Failed to get state: {}", e)))?;

        self.metrics.record_grpc_request("UnsuspendContainer", "ok", start.elapsed());

        Ok(Response::new(UnsuspendContainerResponse {
            success: true,
            state: Some(convert_container_state(&state)),
        }))
    }

    async fn reinstall_container(
        &self,
        request: Request<ReinstallContainerRequest>,
    ) -> std::result::Result<Response<ReinstallContainerResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: ReinstallContainer {}", req.container_id);

        self.manager
            .reinstall_container(&req.container_id, req.preserve_data)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request(
                    "ReinstallContainer",
                    "internal_error",
                    start.elapsed(),
                );
                Status::internal(format!("Failed to reinstall container: {}", e))
            })?;

        let state = self
            .manager
            .get_state(&req.container_id)
            .await
            .map_err(|e| Status::internal(format!("Failed to get state: {}", e)))?;

        self.metrics.record_grpc_request("ReinstallContainer", "ok", start.elapsed());

        Ok(Response::new(ReinstallContainerResponse {
            success: true,
            state: Some(convert_container_state(&state)),
        }))
    }

    // File management endpoints

    async fn list_files(
        &self,
        request: Request<ListFilesRequest>,
    ) -> std::result::Result<Response<ListFilesResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: ListFiles {} path={}", req.container_id, req.path);

        let fm = self.file_manager(&req.container_id);
        let files = fm.list_files(&req.path).await.map_err(|e| {
            self.metrics.record_grpc_request("ListFiles", "error", start.elapsed());
            Status::internal(format!("Failed to list files: {}", e))
        })?;

        let proto_files = files
            .into_iter()
            .map(|f| FileInfo {
                name: f.name,
                path: f.path,
                is_directory: f.is_directory,
                size: f.size,
                modified_at: f.modified_at,
                mime_type: f.mime_type,
                is_symlink: f.is_symlink,
                symlink_target: f.symlink_target,
            })
            .collect();

        self.metrics.record_grpc_request("ListFiles", "ok", start.elapsed());

        Ok(Response::new(ListFilesResponse { files: proto_files }))
    }

    async fn read_file(
        &self,
        request: Request<ReadFileRequest>,
    ) -> std::result::Result<Response<ReadFileResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: ReadFile {} path={}", req.container_id, req.path);

        let fm = self.file_manager(&req.container_id);
        let (content, total_size, mime_type) =
            fm.read_file(&req.path, req.offset, req.length).await.map_err(|e| {
                self.metrics.record_grpc_request("ReadFile", "error", start.elapsed());
                Status::internal(format!("Failed to read file: {}", e))
            })?;

        self.metrics.record_grpc_request("ReadFile", "ok", start.elapsed());

        Ok(Response::new(ReadFileResponse {
            content,
            total_size,
            mime_type,
        }))
    }

    async fn write_file(
        &self,
        request: Request<WriteFileRequest>,
    ) -> std::result::Result<Response<WriteFileResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: WriteFile {} path={}", req.container_id, req.path);

        let fm = self.file_manager(&req.container_id);
        let bytes_written =
            fm.write_file(&req.path, &req.content, req.create_dirs).await.map_err(|e| {
                self.metrics.record_grpc_request("WriteFile", "error", start.elapsed());
                Status::internal(format!("Failed to write file: {}", e))
            })?;

        self.metrics.record_grpc_request("WriteFile", "ok", start.elapsed());

        Ok(Response::new(WriteFileResponse {
            success: true,
            bytes_written,
        }))
    }

    async fn delete_file(
        &self,
        request: Request<DeleteFileRequest>,
    ) -> std::result::Result<Response<DeleteFileResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: DeleteFile {} paths={:?}",
            req.container_id, req.paths
        );

        let fm = self.file_manager(&req.container_id);
        let deleted_count = fm.delete(&req.paths, req.recursive).await.map_err(|e| {
            self.metrics.record_grpc_request("DeleteFile", "error", start.elapsed());
            Status::internal(format!("Failed to delete files: {}", e))
        })?;

        self.metrics.record_grpc_request("DeleteFile", "ok", start.elapsed());

        Ok(Response::new(DeleteFileResponse {
            success: true,
            deleted_count,
        }))
    }

    async fn rename_file(
        &self,
        request: Request<RenameFileRequest>,
    ) -> std::result::Result<Response<RenameFileResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: RenameFile {} {} -> {}",
            req.container_id, req.old_path, req.new_path
        );

        let fm = self.file_manager(&req.container_id);
        fm.rename(&req.old_path, &req.new_path).await.map_err(|e| {
            self.metrics.record_grpc_request("RenameFile", "error", start.elapsed());
            Status::internal(format!("Failed to rename file: {}", e))
        })?;

        self.metrics.record_grpc_request("RenameFile", "ok", start.elapsed());

        Ok(Response::new(RenameFileResponse { success: true }))
    }

    async fn copy_file(
        &self,
        request: Request<CopyFileRequest>,
    ) -> std::result::Result<Response<CopyFileResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: CopyFile {} {} -> {}",
            req.container_id, req.source_path, req.dest_path
        );

        let fm = self.file_manager(&req.container_id);
        fm.copy(&req.source_path, &req.dest_path, req.overwrite).await.map_err(|e| {
            self.metrics.record_grpc_request("CopyFile", "error", start.elapsed());
            Status::internal(format!("Failed to copy file: {}", e))
        })?;

        self.metrics.record_grpc_request("CopyFile", "ok", start.elapsed());

        Ok(Response::new(CopyFileResponse { success: true }))
    }

    async fn create_directory(
        &self,
        request: Request<CreateDirectoryRequest>,
    ) -> std::result::Result<Response<CreateDirectoryResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: CreateDirectory {} path={}",
            req.container_id, req.path
        );

        let fm = self.file_manager(&req.container_id);
        fm.create_directory(&req.path, req.recursive).await.map_err(|e| {
            self.metrics.record_grpc_request("CreateDirectory", "error", start.elapsed());
            Status::internal(format!("Failed to create directory: {}", e))
        })?;

        self.metrics.record_grpc_request("CreateDirectory", "ok", start.elapsed());

        Ok(Response::new(CreateDirectoryResponse { success: true }))
    }

    async fn compress_files(
        &self,
        request: Request<CompressFilesRequest>,
    ) -> std::result::Result<Response<CompressFilesResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: CompressFiles {} -> {}",
            req.container_id, req.output_path
        );

        let fm = self.file_manager(&req.container_id);
        let (archive_path, archive_size) =
            fm.compress(&req.paths, &req.output_path, &req.format).await.map_err(|e| {
                self.metrics.record_grpc_request("CompressFiles", "error", start.elapsed());
                Status::internal(format!("Failed to compress files: {}", e))
            })?;

        self.metrics.record_grpc_request("CompressFiles", "ok", start.elapsed());

        Ok(Response::new(CompressFilesResponse {
            success: true,
            archive_path,
            archive_size,
        }))
    }

    async fn decompress_file(
        &self,
        request: Request<DecompressFileRequest>,
    ) -> std::result::Result<Response<DecompressFileResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: DecompressFile {} {} -> {}",
            req.container_id, req.archive_path, req.output_dir
        );

        let fm = self.file_manager(&req.container_id);
        let files_extracted = fm
            .decompress(&req.archive_path, &req.output_dir, req.overwrite)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("DecompressFile", "error", start.elapsed());
                Status::internal(format!("Failed to decompress file: {}", e))
            })?;

        self.metrics.record_grpc_request("DecompressFile", "ok", start.elapsed());

        Ok(Response::new(DecompressFileResponse {
            success: true,
            files_extracted,
        }))
    }

    type DownloadFileStream =
        tokio_stream::wrappers::ReceiverStream<std::result::Result<FileChunk, Status>>;

    async fn download_file(
        &self,
        request: Request<DownloadFileRequest>,
    ) -> std::result::Result<Response<Self::DownloadFileStream>, Status> {
        let req = request.into_inner();
        let container_id = req.container_id.clone();
        let path = req.path.clone();

        info!("gRPC: DownloadFile {} path={}", container_id, path);

        let fm = self.file_manager(&container_id);
        let file_path = fm.server_dir().join(path.trim_start_matches('/'));

        if !file_path.exists() {
            return Err(Status::not_found(format!("File not found: {}", path)));
        }

        let metadata = tokio::fs::metadata(&file_path)
            .await
            .map_err(|e| Status::internal(format!("Failed to get file metadata: {}", e)))?;
        let total_size = metadata.len();

        let (tx, rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            let mut file = match tokio::fs::File::open(&file_path).await {
                Ok(f) => f,
                Err(e) => {
                    let _ =
                        tx.send(Err(Status::internal(format!("Failed to open file: {}", e)))).await;
                    return;
                }
            };

            let mut offset: u64 = 0;
            let mut buffer = vec![0u8; STREAM_CHUNK_SIZE];

            loop {
                let n = match file.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(format!("Read error: {}", e)))).await;
                        return;
                    }
                };

                let is_last = offset + n as u64 >= total_size;

                let chunk = FileChunk {
                    container_id: container_id.clone(),
                    path: path.clone(),
                    data: buffer[..n].to_vec(),
                    offset,
                    total_size,
                    is_last,
                };

                offset += n as u64;

                if tx.send(Ok(chunk)).await.is_err() {
                    break;
                }

                if is_last {
                    break;
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    async fn upload_file(
        &self,
        request: Request<tonic::Streaming<FileChunk>>,
    ) -> std::result::Result<Response<UploadFileResponse>, Status> {
        let start = Instant::now();
        let mut stream = request.into_inner();

        // Read the first chunk to get container_id and path
        let first_chunk = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("Failed to read first chunk: {}", e)))?
            .ok_or_else(|| Status::invalid_argument("No data received"))?;

        let container_id = first_chunk.container_id.clone();
        let path = first_chunk.path.clone();

        info!("gRPC: UploadFile {} path={}", container_id, path);

        let fm = self.file_manager(&container_id);

        // Collect all data
        let mut data = first_chunk.data;

        if !first_chunk.is_last {
            while let Some(chunk) = stream
                .message()
                .await
                .map_err(|e| Status::internal(format!("Failed to read chunk: {}", e)))?
            {
                data.extend_from_slice(&chunk.data);
                if chunk.is_last {
                    break;
                }
            }
        }

        let bytes_written = fm.write_file(&path, &data, true).await.map_err(|e| {
            self.metrics.record_grpc_request("UploadFile", "error", start.elapsed());
            Status::internal(format!("Failed to write uploaded file: {}", e))
        })?;

        self.metrics.record_grpc_request("UploadFile", "ok", start.elapsed());

        Ok(Response::new(UploadFileResponse {
            success: true,
            bytes_written,
            path,
        }))
    }

    // Backup management endpoints

    async fn create_backup(
        &self,
        request: Request<CreateBackupRequest>,
    ) -> std::result::Result<Response<CreateBackupResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: CreateBackup {} name={}", req.container_id, req.name);

        let backup = self
            .backup_manager
            .create_backup(
                &req.container_id,
                &req.name,
                &req.include_paths,
                &req.exclude_paths,
            )
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("CreateBackup", "error", start.elapsed());
                Status::internal(format!("Failed to create backup: {}", e))
            })?;

        self.metrics.record_grpc_request("CreateBackup", "ok", start.elapsed());

        Ok(Response::new(CreateBackupResponse {
            success: true,
            backup: Some(convert_backup_info(&backup)),
        }))
    }

    async fn list_backups(
        &self,
        request: Request<ListBackupsRequest>,
    ) -> std::result::Result<Response<ListBackupsResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: ListBackups {}", req.container_id);

        let backups = self.backup_manager.list_backups(&req.container_id).await.map_err(|e| {
            self.metrics.record_grpc_request("ListBackups", "error", start.elapsed());
            Status::internal(format!("Failed to list backups: {}", e))
        })?;

        let proto_backups = backups.iter().map(convert_backup_info).collect();

        self.metrics.record_grpc_request("ListBackups", "ok", start.elapsed());

        Ok(Response::new(ListBackupsResponse {
            backups: proto_backups,
        }))
    }

    async fn restore_backup(
        &self,
        request: Request<RestoreBackupRequest>,
    ) -> std::result::Result<Response<RestoreBackupResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: RestoreBackup {} backup={}",
            req.container_id, req.backup_id
        );

        // Stop container if requested
        if req.stop_container {
            let _ = self.manager.stop_container(&req.container_id, Some(30)).await;
        }

        self.backup_manager
            .restore_backup(&req.container_id, &req.backup_id, req.delete_existing)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("RestoreBackup", "error", start.elapsed());
                Status::internal(format!("Failed to restore backup: {}", e))
            })?;

        self.metrics.record_grpc_request("RestoreBackup", "ok", start.elapsed());

        Ok(Response::new(RestoreBackupResponse {
            success: true,
            error: None,
        }))
    }

    async fn delete_backup(
        &self,
        request: Request<DeleteBackupRequest>,
    ) -> std::result::Result<Response<DeleteBackupResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: DeleteBackup {} backup={}",
            req.container_id, req.backup_id
        );

        self.backup_manager
            .delete_backup(&req.container_id, &req.backup_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("DeleteBackup", "error", start.elapsed());
                Status::internal(format!("Failed to delete backup: {}", e))
            })?;

        self.metrics.record_grpc_request("DeleteBackup", "ok", start.elapsed());

        Ok(Response::new(DeleteBackupResponse { success: true }))
    }

    type DownloadBackupStream =
        tokio_stream::wrappers::ReceiverStream<std::result::Result<FileChunk, Status>>;

    async fn download_backup(
        &self,
        request: Request<DownloadBackupRequest>,
    ) -> std::result::Result<Response<Self::DownloadBackupStream>, Status> {
        let req = request.into_inner();
        let container_id = req.container_id.clone();
        let backup_id = req.backup_id.clone();

        info!("gRPC: DownloadBackup {} backup={}", container_id, backup_id);

        // Verify backup exists
        let _backup = self
            .backup_manager
            .get_backup(&container_id, &backup_id)
            .await
            .map_err(|e| Status::not_found(format!("Backup not found: {}", e)))?;

        let backup_path = self.backup_manager.get_backup_path(&container_id, &backup_id);

        if !backup_path.exists() {
            return Err(Status::not_found("Backup file not found on disk"));
        }

        let metadata = tokio::fs::metadata(&backup_path)
            .await
            .map_err(|e| Status::internal(format!("Failed to get backup metadata: {}", e)))?;
        let total_size = metadata.len();

        let (tx, rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            let mut file = match tokio::fs::File::open(&backup_path).await {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx
                        .send(Err(Status::internal(format!(
                            "Failed to open backup: {}",
                            e
                        ))))
                        .await;
                    return;
                }
            };

            let mut offset: u64 = 0;
            let mut buffer = vec![0u8; STREAM_CHUNK_SIZE];

            loop {
                let n = match file.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(format!("Read error: {}", e)))).await;
                        return;
                    }
                };

                let is_last = offset + n as u64 >= total_size;

                let chunk = FileChunk {
                    container_id: container_id.clone(),
                    path: backup_id.clone(),
                    data: buffer[..n].to_vec(),
                    offset,
                    total_size,
                    is_last,
                };

                offset += n as u64;

                if tx.send(Ok(chunk)).await.is_err() {
                    break;
                }

                if is_last {
                    break;
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    // Schedule management endpoints

    async fn create_schedule(
        &self,
        request: Request<CreateScheduleRequest>,
    ) -> std::result::Result<Response<CreateScheduleResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: CreateSchedule {} name={}",
            req.container_id, req.name
        );

        let tasks: Vec<InternalScheduleTask> = req
            .tasks
            .iter()
            .map(|t| InternalScheduleTask {
                task_type: InternalScheduleTaskType::from_i32(t.task_type),
                time_offset: t.time_offset,
                payload: t.payload.clone(),
            })
            .collect();

        let schedule = self
            .schedule_manager
            .create_schedule(
                &req.container_id,
                &req.name,
                &req.cron_expression,
                req.is_active,
                tasks,
            )
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("CreateSchedule", "error", start.elapsed());
                Status::invalid_argument(format!("Failed to create schedule: {}", e))
            })?;

        self.metrics.record_grpc_request("CreateSchedule", "ok", start.elapsed());

        Ok(Response::new(CreateScheduleResponse {
            success: true,
            schedule: Some(convert_schedule_info(&schedule)),
        }))
    }

    async fn list_schedules(
        &self,
        request: Request<ListSchedulesRequest>,
    ) -> std::result::Result<Response<ListSchedulesResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!("gRPC: ListSchedules {}", req.container_id);

        let schedules =
            self.schedule_manager.list_schedules(&req.container_id).await.map_err(|e| {
                self.metrics.record_grpc_request("ListSchedules", "error", start.elapsed());
                Status::internal(format!("Failed to list schedules: {}", e))
            })?;

        let proto_schedules = schedules.iter().map(convert_schedule_info).collect();

        self.metrics.record_grpc_request("ListSchedules", "ok", start.elapsed());

        Ok(Response::new(ListSchedulesResponse {
            schedules: proto_schedules,
        }))
    }

    async fn update_schedule(
        &self,
        request: Request<UpdateScheduleRequest>,
    ) -> std::result::Result<Response<UpdateScheduleResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: UpdateSchedule {} schedule={}",
            req.container_id, req.schedule_id
        );

        let tasks = if req.tasks.is_empty() {
            None
        } else {
            Some(
                req.tasks
                    .iter()
                    .map(|t| InternalScheduleTask {
                        task_type: InternalScheduleTaskType::from_i32(t.task_type),
                        time_offset: t.time_offset,
                        payload: t.payload.clone(),
                    })
                    .collect(),
            )
        };

        let schedule = self
            .schedule_manager
            .update_schedule(
                &req.container_id,
                &req.schedule_id,
                req.name.as_deref(),
                req.cron_expression.as_deref(),
                req.is_active,
                tasks,
            )
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("UpdateSchedule", "error", start.elapsed());
                Status::internal(format!("Failed to update schedule: {}", e))
            })?;

        self.metrics.record_grpc_request("UpdateSchedule", "ok", start.elapsed());

        Ok(Response::new(UpdateScheduleResponse {
            success: true,
            schedule: Some(convert_schedule_info(&schedule)),
        }))
    }

    async fn delete_schedule(
        &self,
        request: Request<DeleteScheduleRequest>,
    ) -> std::result::Result<Response<DeleteScheduleResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: DeleteSchedule {} schedule={}",
            req.container_id, req.schedule_id
        );

        self.schedule_manager
            .delete_schedule(&req.container_id, &req.schedule_id)
            .await
            .map_err(|e| {
                self.metrics.record_grpc_request("DeleteSchedule", "error", start.elapsed());
                Status::internal(format!("Failed to delete schedule: {}", e))
            })?;

        self.metrics.record_grpc_request("DeleteSchedule", "ok", start.elapsed());

        Ok(Response::new(DeleteScheduleResponse { success: true }))
    }

    async fn trigger_schedule(
        &self,
        request: Request<TriggerScheduleRequest>,
    ) -> std::result::Result<Response<TriggerScheduleResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        info!(
            "gRPC: TriggerSchedule {} schedule={}",
            req.container_id, req.schedule_id
        );

        // Create a simple callback that sends commands via the container manager
        let manager = self.manager.clone();
        let callback: crate::schedule::ScheduleCallback = Arc::new(move |container_id, task| {
            let manager = manager.clone();
            let container_id = container_id.to_string();
            let task = task.clone();
            Box::pin(async move {
                match task.task_type {
                    InternalScheduleTaskType::Command => {
                        manager.send_command(&container_id, &task.payload).await
                    }
                    InternalScheduleTaskType::Power => match task.payload.as_str() {
                        "start" => manager.start_container(&container_id).await,
                        "stop" => manager.stop_container(&container_id, Some(30)).await,
                        "restart" => manager.restart_container(&container_id).await,
                        "kill" => manager.stop_container(&container_id, Some(0)).await,
                        other => Err(crate::error::NodeError::InvalidInput(format!(
                            "Unknown power action: {}",
                            other
                        ))),
                    },
                    InternalScheduleTaskType::Backup => {
                        // Backup tasks require a backup manager reference;
                        // for manual triggers, just log a warning
                        tracing::warn!(
                            "Backup task triggered manually; use CreateBackup RPC instead"
                        );
                        Ok(())
                    }
                }
            })
        });

        match self
            .schedule_manager
            .trigger_schedule(&req.container_id, &req.schedule_id, &callback)
            .await
        {
            Ok(()) => {
                self.metrics.record_grpc_request("TriggerSchedule", "ok", start.elapsed());
                Ok(Response::new(TriggerScheduleResponse {
                    success: true,
                    error: None,
                }))
            }
            Err(e) => {
                self.metrics.record_grpc_request("TriggerSchedule", "error", start.elapsed());
                Ok(Response::new(TriggerScheduleResponse {
                    success: false,
                    error: Some(format!("{}", e)),
                }))
            }
        }
    }
}

/// Convert internal BackupInfo to protobuf BackupInfo
fn convert_backup_info(info: &crate::backup::BackupInfo) -> super::proto::BackupInfo {
    use crate::backup::BackupStatus as InternalBackupStatus;

    let status = match info.status {
        InternalBackupStatus::Pending => BackupStatus::BackupPending,
        InternalBackupStatus::InProgress => BackupStatus::BackupInProgress,
        InternalBackupStatus::Completed => BackupStatus::BackupCompleted,
        InternalBackupStatus::Failed => BackupStatus::BackupFailed,
    };

    super::proto::BackupInfo {
        id: info.id.clone(),
        container_id: info.container_id.clone(),
        name: info.name.clone(),
        size: info.size,
        created_at: info.created_at,
        checksum: info.checksum.clone(),
        status: status as i32,
        error: info.error.clone(),
    }
}

/// Convert internal ScheduleInfo to protobuf ScheduleInfo
fn convert_schedule_info(info: &crate::schedule::ScheduleInfo) -> super::proto::ScheduleInfo {
    let tasks = info
        .tasks
        .iter()
        .map(|t| super::proto::ScheduleTask {
            task_type: t.task_type.to_i32(),
            time_offset: t.time_offset,
            payload: t.payload.clone(),
        })
        .collect();

    super::proto::ScheduleInfo {
        id: info.id.clone(),
        container_id: info.container_id.clone(),
        name: info.name.clone(),
        cron_expression: info.cron_expression.clone(),
        is_active: info.is_active,
        created_at: info.created_at,
        last_run_at: info.last_run_at,
        next_run_at: info.next_run_at,
        tasks,
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
        InternalStatus::Suspended => ContainerStatus::StatusSuspended,
    };

    ContainerState {
        id: state.id.clone(),
        name: state.name.clone(),
        status: status as i32,
        pid: state.pid,
        exit_code: state.exit_code,
        image: state.image.clone(),
        created_at: state.created_at.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
        started_at: state
            .started_at
            .map(|t| t.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64),
        stopped_at: state
            .stopped_at
            .map(|t| t.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64),
        restart_count: state.restart_count,
    }
}

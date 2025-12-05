use super::*;
use crate::error::{NodeError, Result};
use async_trait::async_trait;
use containerd_client::{
    services::v1::{
        containers_client::ContainersClient,
        images_client::ImagesClient,
        tasks_client::TasksClient,
        Container as ContainerdContainer,
        GetContainerRequest, GetImageRequest, ListTasksRequest,
        CreateTaskRequest, StartRequest, KillRequest, DeleteTaskRequest,
        CreateContainerRequest, DeleteContainerRequest,
    },
    tonic::{transport::Channel, Request},
    with_namespace,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Containerd container runtime implementation
pub struct ContainerdRuntime {
    channel: Arc<RwLock<Option<Channel>>>,
    socket_path: String,
    namespace: String,
}

impl ContainerdRuntime {
    /// Create a new Containerd runtime
    pub fn new(socket_path: String, namespace: String) -> Self {
        Self {
            channel: Arc::new(RwLock::new(None)),
            socket_path,
            namespace,
        }
    }

    /// Connect to Containerd
    pub async fn connect(&self) -> Result<()> {
        info!(
            "Connecting to Containerd at {} (namespace: {})",
            self.socket_path, self.namespace
        );

        // Containerd uses Unix domain sockets, prepend "unix://" scheme
        let endpoint = if self.socket_path.starts_with("unix://") {
            self.socket_path.clone()
        } else {
            format!("unix://{}", self.socket_path)
        };

        // Connect to Containerd via Unix socket
        let channel = Channel::from_shared(endpoint)
            .map_err(|e| NodeError::ContainerdError(format!("Invalid endpoint: {}", e)))?
            .connect()
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to connect: {}", e)))?;

        let mut guard = self.channel.write().await;
        *guard = Some(channel);

        info!("Successfully connected to Containerd");
        Ok(())
    }

    /// Get or create a connected channel
    async fn get_channel(&self) -> Result<Channel> {
        let channel = self.channel.read().await;
        if let Some(ref ch) = *channel {
            return Ok(ch.clone());
        }

        drop(channel); // Release read lock before connecting
        self.connect().await?;

        let channel = self.channel.read().await;
        channel
            .as_ref()
            .cloned()
            .ok_or_else(|| NodeError::ContainerdError("Failed to connect".to_string()))
    }

    /// Create containers client
    async fn containers_client(&self) -> Result<ContainersClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ContainersClient::new(channel))
    }

    /// Create images client
    async fn images_client(&self) -> Result<ImagesClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ImagesClient::new(channel))
    }

    /// Create tasks client
    async fn tasks_client(&self) -> Result<TasksClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(TasksClient::new(channel))
    }
}

#[async_trait]
impl ContainerRuntime for ContainerdRuntime {
    async fn pull_image(&self, image: &str) -> Result<()> {
        info!("[Containerd] Checking image: {}", image);

        let mut client = self.images_client().await?;

        // Check if image already exists
        let req = with_namespace!(GetImageRequest {
            name: image.to_string(),
        }, &self.namespace);

        match client.get(req).await {
            Ok(_) => {
                debug!("Image {} already exists", image);
                Ok(())
            }
            Err(status) => {
                if status.code() == containerd_client::tonic::Code::NotFound {
                    Err(NodeError::ContainerdError(format!(
                        "Image {} not found. Please pull it manually using: ctr -n {} images pull {}",
                        image, self.namespace, image
                    )))
                } else {
                    Err(NodeError::ContainerdError(format!(
                        "Failed to check image: {}",
                        status
                    )))
                }
            }
        }

        // NOTE: Image pulling in containerd is complex and requires:
        // 1. Using the content service to download layers
        // 2. Using the snapshots service to unpack layers
        // 3. Using the images service to register the image
        // For now, we expect images to be pulled manually via `ctr images pull`
        // This is acceptable for an MVP deployment where operators control the node
    }

    async fn create(&self, id: &str, spec: ContainerSpec) -> Result<ContainerInfo> {
        info!("[Containerd] Creating container: {}", id);

        let mut client = self.containers_client().await?;

        // Build OCI spec from ContainerSpec
        let oci_spec = spec_to_oci(&spec)?;

        // Create container
        let container = ContainerdContainer {
            id: id.to_string(),
            image: spec.image.clone(),
            runtime: Some(containerd_client::services::v1::container::Runtime {
                name: "io.containerd.runc.v2".to_string(),
                options: None,
            }),
            spec: Some(prost_types::Any {
                type_url: "types.containerd.io/opencontainers/runtime-spec/1/Spec".to_string(),
                value: oci_spec.into_bytes(),
            }),
            ..Default::default()
        };

        let req = with_namespace!(CreateContainerRequest {
            container: Some(container),
        }, &self.namespace);

        client
            .create(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to create container: {}", e)))?;

        info!("Successfully created container: {}", id);

        Ok(ContainerInfo {
            id: id.to_string(),
            pid: None,
            status: "created".to_string(),
            exit_code: None,
        })
    }

    async fn start(&self, id: &str) -> Result<u32> {
        info!("[Containerd] Starting container: {}", id);

        let mut tasks_client = self.tasks_client().await?;

        // Create task for the container
        let req = with_namespace!(CreateTaskRequest {
            container_id: id.to_string(),
            ..Default::default()
        }, &self.namespace);

        let task_response = tasks_client
            .create(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to create task: {}", e)))?
            .into_inner();

        let pid = task_response.pid;

        // Start the task
        let req = with_namespace!(StartRequest {
            container_id: id.to_string(),
            ..Default::default()
        }, &self.namespace);

        tasks_client
            .start(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to start task: {}", e)))?;

        info!("Successfully started container: {} (PID: {})", id, pid);
        Ok(pid)
    }

    async fn stop(&self, id: &str, timeout_secs: u32) -> Result<i32> {
        info!("[Containerd] Stopping container: {} (timeout: {}s)", id, timeout_secs);

        let mut tasks_client = self.tasks_client().await?;

        // Send SIGTERM to gracefully stop
        let req = with_namespace!(KillRequest {
            container_id: id.to_string(),
            signal: 15, // SIGTERM
            ..Default::default()
        }, &self.namespace);

        if let Err(e) = tasks_client.kill(req).await {
            warn!("Failed to send SIGTERM to container {}: {}", id, e);
        }

        // Wait for task to exit (with timeout)
        let timeout = tokio::time::Duration::from_secs(timeout_secs as u64);
        let start_time = tokio::time::Instant::now();

        let exit_code = loop {
            // Check if task still exists
            let req = with_namespace!(ListTasksRequest {
                filter: format!("id=={}", id),
            }, &self.namespace);

            let response = tasks_client
                .list(req)
                .await
                .map_err(|e| NodeError::ContainerdError(format!("Failed to list tasks: {}", e)))?
                .into_inner();

            if response.tasks.is_empty() {
                // Task no longer exists, assume exit code 0
                break 0;
            }

            let task = &response.tasks[0];
            // Check if task status is stopped (status field is i32, not enum)
            // Status values: Unknown=0, Created=1, Running=2, Stopped=3, Paused=4, Pausing=5
            if task.status == 3 { // Stopped
                break task.exit_status as i32;
            }

            // Check timeout
            if start_time.elapsed() >= timeout {
                warn!("Container {} did not stop within {}s, sending SIGKILL", id, timeout_secs);

                // Force kill with SIGKILL
                let req = with_namespace!(KillRequest {
                    container_id: id.to_string(),
                    signal: 9, // SIGKILL
                    ..Default::default()
                }, &self.namespace);

                tasks_client
                    .kill(req)
                    .await
                    .map_err(|e| NodeError::ContainerdError(format!("Failed to send SIGKILL: {}", e)))?;

                break 137; // Standard exit code for SIGKILL
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        };

        // Delete the task
        let req = with_namespace!(DeleteTaskRequest {
            container_id: id.to_string(),
        }, &self.namespace);

        tasks_client
            .delete(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to delete task: {}", e)))?;

        info!("Successfully stopped container: {} (exit code: {})", id, exit_code);
        Ok(exit_code)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        info!("[Containerd] Deleting container: {}", id);

        let mut tasks_client = self.tasks_client().await?;
        let mut containers_client = self.containers_client().await?;

        // Try to delete task first (if it exists)
        let req = with_namespace!(DeleteTaskRequest {
            container_id: id.to_string(),
        }, &self.namespace);

        if let Err(e) = tasks_client.delete(req).await {
            debug!("Failed to delete task (may not exist): {}", e);
        }

        // Delete the container
        let req = with_namespace!(DeleteContainerRequest {
            id: id.to_string(),
        }, &self.namespace);

        containers_client
            .delete(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to delete container: {}", e)))?;

        info!("Successfully deleted container: {}", id);
        Ok(())
    }

    async fn inspect(&self, id: &str) -> Result<ContainerInfo> {
        info!("[Containerd] Inspecting container: {}", id);

        let mut containers_client = self.containers_client().await?;
        let mut tasks_client = self.tasks_client().await?;

        // Get container info
        let req = with_namespace!(GetContainerRequest {
            id: id.to_string(),
        }, &self.namespace);

        let _container = containers_client
            .get(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Container not found: {}", e)))?
            .into_inner();

        // Get task info (if exists)
        let req = with_namespace!(ListTasksRequest {
            filter: format!("id=={}", id),
        }, &self.namespace);

        let tasks_response = tasks_client
            .list(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to list tasks: {}", e)))?
            .into_inner();

        let (pid, status, exit_code) = if let Some(task) = tasks_response.tasks.first() {
            // Status values: Unknown=0, Created=1, Running=2, Stopped=3, Paused=4, Pausing=5
            let status_str = match task.status {
                0 => "unknown",
                1 => "created",
                2 => "running",
                3 => "stopped",
                4 => "paused",
                5 => "pausing",
                _ => "unknown",
            };

            let pid = if task.pid > 0 { Some(task.pid) } else { None };
            let exit_code = if task.status == 3 { // Stopped
                Some(task.exit_status as i32)
            } else {
                None
            };

            (pid, status_str.to_string(), exit_code)
        } else {
            (None, "created".to_string(), None)
        };

        Ok(ContainerInfo {
            id: id.to_string(),
            pid,
            status,
            exit_code,
        })
    }

    async fn attach(&self, id: &str) -> Result<Box<dyn ConsoleStream>> {
        info!("[Containerd] Attaching to container: {}", id);

        // For now, return a placeholder stream
        // Real implementation would use containerd's attach API
        // This requires more complex stream handling with containerd's I/O pipes

        Ok(Box::new(ContainerdConsoleStream::new(id)))
    }
}

// Helper functions for converting between our types and Containerd types

/// Convert ContainerSpec to OCI runtime spec JSON
fn spec_to_oci(spec: &ContainerSpec) -> Result<String> {
    use serde_json::json;

    // Build full command from command + args
    let mut process_args = spec.command.clone();
    process_args.extend(spec.args.clone());

    // Convert environment variables to "KEY=VALUE" format
    let env: Vec<String> = spec
        .env
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();

    // Build Linux resources
    let cpu_shares = spec.resources.cpu_shares;
    let memory_limit = spec.resources.memory_bytes as i64;

    // Build mounts
    let mounts: Vec<serde_json::Value> = spec
        .mounts
        .iter()
        .map(|m| {
            json!({
                "destination": m.target,
                "type": "bind",
                "source": m.source,
                "options": if m.read_only { vec!["rbind", "ro"] } else { vec!["rbind", "rw"] }
            })
        })
        .collect();

    // Create minimal OCI spec
    let oci_spec = json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": {
                "uid": 0,
                "gid": 0
            },
            "args": process_args,
            "env": env,
            "cwd": spec.working_dir,
            "capabilities": {
                "bounding": ["CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_SETGID", "CAP_SETUID", "CAP_NET_BIND_SERVICE"],
                "effective": ["CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_SETGID", "CAP_SETUID", "CAP_NET_BIND_SERVICE"],
                "permitted": ["CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_SETGID", "CAP_SETUID", "CAP_NET_BIND_SERVICE"],
            },
            "rlimits": [
                {
                    "type": "RLIMIT_NOFILE",
                    "hard": 1024,
                    "soft": 1024
                }
            ]
        },
        "root": {
            "path": "rootfs",
            "readonly": false
        },
        "hostname": "container",
        "mounts": mounts,
        "linux": {
            "resources": {
                "cpu": {
                    "shares": cpu_shares
                },
                "memory": {
                    "limit": memory_limit
                }
            },
            "namespaces": [
                {"type": "pid"},
                {"type": "ipc"},
                {"type": "uts"},
                {"type": "mount"},
                {"type": "network"}
            ]
        }
    });

    serde_json::to_string(&oci_spec)
        .map_err(|e| NodeError::Internal(format!("Failed to serialize OCI spec: {}", e)))
}

/// Placeholder console stream for Containerd
/// Real implementation would use containerd's attach/exec API
struct ContainerdConsoleStream {
    _container_id: String,
}

impl ContainerdConsoleStream {
    fn new(container_id: &str) -> Self {
        Self {
            _container_id: container_id.to_string(),
        }
    }
}

#[async_trait]
impl ConsoleStream for ContainerdConsoleStream {
    async fn read_line(&mut self) -> Result<Option<String>> {
        // TODO: Implement actual console streaming
        // This would require:
        // 1. Attaching to task's I/O
        // 2. Reading from stdout/stderr streams
        // 3. Handling EOF and errors

        Ok(None) // Return EOF for now
    }
}

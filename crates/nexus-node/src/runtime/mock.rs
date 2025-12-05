use super::*;
use crate::error::{NodeError, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;

/// Mock container runtime for testing
pub struct MockRuntime {
    containers: Arc<RwLock<HashMap<String, MockContainer>>>,
}

#[derive(Clone)]
struct MockContainer {
    id: String,
    #[allow(dead_code)]
    spec: ContainerSpec,
    pid: Option<u32>,
    status: String,
    exit_code: Option<i32>,
}

impl MockRuntime {
    pub fn new() -> Self {
        Self {
            containers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl ContainerRuntime for MockRuntime {
    async fn pull_image(&self, image: &str) -> Result<()> {
        tracing::info!("[MOCK] Pulling image: {}", image);
        // Simulate image pull
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        Ok(())
    }

    async fn create(&self, id: &str, spec: ContainerSpec) -> Result<ContainerInfo> {
        tracing::info!("[MOCK] Creating container: {}", id);

        let container = MockContainer {
            id: id.to_string(),
            spec,
            pid: None,
            status: "created".to_string(),
            exit_code: None,
        };

        let mut containers = self.containers.write().await;
        containers.insert(id.to_string(), container);

        Ok(ContainerInfo {
            id: id.to_string(),
            pid: None,
            status: "created".to_string(),
            exit_code: None,
        })
    }

    async fn start(&self, id: &str) -> Result<u32> {
        tracing::info!("[MOCK] Starting container: {}", id);

        let mut containers = self.containers.write().await;
        let container = containers
            .get_mut(id)
            .ok_or_else(|| NodeError::ContainerNotFound(id.to_string()))?;

        // Simulate PID assignment
        let pid = std::process::id();
        container.pid = Some(pid);
        container.status = "running".to_string();

        Ok(pid)
    }

    async fn stop(&self, id: &str, _timeout_secs: u32) -> Result<i32> {
        tracing::info!("[MOCK] Stopping container: {}", id);

        let mut containers = self.containers.write().await;
        let container = containers
            .get_mut(id)
            .ok_or_else(|| NodeError::ContainerNotFound(id.to_string()))?;

        container.pid = None;
        container.status = "stopped".to_string();
        container.exit_code = Some(0);

        Ok(0)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[MOCK] Deleting container: {}", id);

        let mut containers = self.containers.write().await;
        containers.remove(id);

        Ok(())
    }

    async fn inspect(&self, id: &str) -> Result<ContainerInfo> {
        let containers = self.containers.read().await;
        let container = containers
            .get(id)
            .ok_or_else(|| NodeError::ContainerNotFound(id.to_string()))?;

        Ok(ContainerInfo {
            id: container.id.clone(),
            pid: container.pid,
            status: container.status.clone(),
            exit_code: container.exit_code,
        })
    }

    async fn attach(&self, id: &str) -> Result<Box<dyn ConsoleStream>> {
        tracing::info!("[MOCK] Attaching to container: {}", id);

        // Check container exists
        let containers = self.containers.read().await;
        if !containers.contains_key(id) {
            return Err(NodeError::ContainerNotFound(id.to_string()));
        }

        Ok(Box::new(MockConsoleStream::new(id)))
    }
}

pub struct MockConsoleStream {
    container_id: String,
    line_count: usize,
    follow: bool,
}

impl MockConsoleStream {
    fn new(container_id: &str) -> Self {
        Self {
            container_id: container_id.to_string(),
            line_count: 0,
            follow: false,
        }
    }

    pub fn with_follow(container_id: &str, follow: bool) -> Self {
        Self {
            container_id: container_id.to_string(),
            line_count: 0,
            follow,
        }
    }
}

#[async_trait]
impl ConsoleStream for MockConsoleStream {
    async fn read_line(&mut self) -> Result<Option<String>> {
        // Generate some initial lines
        if self.line_count < 10 {
            self.line_count += 1;

            let line = match self.line_count {
                1 => format!("[{}] Server starting...", self.container_id),
                2 => format!("[{}] Loading configuration", self.container_id),
                3 => format!("[{}] Initializing game world", self.container_id),
                4 => format!("[{}] Starting network listener on port 25565", self.container_id),
                5 => format!("[{}] Server ready!", self.container_id),
                6 => format!("[{}] Waiting for players...", self.container_id),
                7 => format!("[{}] Player joined: TestPlayer", self.container_id),
                8 => format!("[{}] <TestPlayer> Hello world!", self.container_id),
                9 => format!("[{}] Autosaving world...", self.container_id),
                10 => format!("[{}] Save complete", self.container_id),
                _ => return Ok(None),
            };

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            return Ok(Some(line));
        }

        // If following, generate periodic logs
        if self.follow {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            self.line_count += 1;
            let line = format!(
                "[{}] Periodic log message #{}",
                self.container_id,
                self.line_count - 10
            );
            Ok(Some(line))
        } else {
            // EOF - no more logs
            Ok(None)
        }
    }
}

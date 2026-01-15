use thiserror::Error;

/// Errors that can occur in Nexus Node operations
#[derive(Error, Debug)]
pub enum NodeError {
    #[error("Container not found: {0}")]
    ContainerNotFound(String),

    #[error("Container already exists: {0}")]
    ContainerAlreadyExists(String),

    #[error("Container is not running: {0}")]
    ContainerNotRunning(String),

    #[error("Failed to load config: {path}")]
    ConfigLoadError {
        path: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("Invalid config: {reason}")]
    InvalidConfig { reason: String },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Containerd error: {0}")]
    ContainerdError(String),

    #[error("Container start failed: {container_id}")]
    StartFailed {
        container_id: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("Container stop failed: {container_id}")]
    StopFailed {
        container_id: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerdeError(#[from] serde_yaml::Error),

    #[error("Metrics error: {0}")]
    MetricsError(#[from] prometheus::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, NodeError>;

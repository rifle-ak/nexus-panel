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

    // The cause is part of the message on purpose: this error is what the
    // panel shows an operator, and "start failed: <uuid>" on its own tells
    // them nothing about what to fix.
    #[error("Container start failed: {container_id}: {source}")]
    StartFailed {
        container_id: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("Container stop failed: {container_id}: {source}")]
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

    #[error("Archive error: {0}")]
    ArchiveError(String),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl From<zip::result::ZipError> for NodeError {
    fn from(e: zip::result::ZipError) -> Self {
        NodeError::ArchiveError(e.to_string())
    }
}

impl From<std::path::StripPrefixError> for NodeError {
    fn from(e: std::path::StripPrefixError) -> Self {
        NodeError::InvalidInput(e.to_string())
    }
}

impl From<walkdir::Error> for NodeError {
    fn from(e: walkdir::Error) -> Self {
        NodeError::IoError(std::io::Error::other(e))
    }
}

impl From<tokio::task::JoinError> for NodeError {
    fn from(e: tokio::task::JoinError) -> Self {
        NodeError::Internal(format!("Task join error: {}", e))
    }
}

pub type Result<T> = std::result::Result<T, NodeError>;

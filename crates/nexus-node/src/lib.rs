//! Nexus Node - Container runtime daemon for Nexus Panel
//!
//! Manages game server containers using Containerd, providing lifecycle management,
//! resource monitoring, and gRPC control API.

pub mod config;
pub mod container;
pub mod error;
pub mod grpc;
pub mod health;
pub mod metrics;
pub mod metrics_server;
pub mod runtime;
pub mod secrets;

pub use config::load_config;
pub use container::{ContainerManager, ContainerState, ContainerStatus};
pub use error::{NodeError, Result};
pub use grpc::server::NodeServiceImpl;
pub use health::{HealthChecker, HealthCheckResult, HealthStatus};
pub use metrics::Metrics;
pub use metrics_server::start_metrics_server;
pub use runtime::{
    ContainerRuntime,
    containerd::ContainerdRuntime,
    mock::MockRuntime,
};
pub use container::state::ContainerStatus;
pub use secrets::{SecretsManager, SecretResolver, EnvSecretsManager, MemorySecretsManager};

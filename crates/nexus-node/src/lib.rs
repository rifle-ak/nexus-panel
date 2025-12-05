//! Nexus Node - Container runtime daemon for Nexus Panel
//!
//! Manages game server containers using Containerd, providing lifecycle management,
//! resource monitoring, and gRPC control API.

pub mod config;
pub mod container;
pub mod error;
pub mod grpc;
pub mod runtime;

pub use config::load_config;
pub use container::{ContainerManager, ContainerState, ContainerStatus};
pub use error::{NodeError, Result};
pub use grpc::server::NodeServiceImpl;
pub use runtime::{
    ContainerRuntime,
    containerd::ContainerdRuntime,
    mock::MockRuntime,
};

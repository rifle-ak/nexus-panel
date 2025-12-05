//! Nexus Node - Container runtime daemon for Nexus Panel
//!
//! Manages game server containers using Containerd, providing lifecycle management,
//! resource monitoring, and gRPC control API.

pub mod config;
pub mod container;
pub mod error;
pub mod runtime;

pub use config::load_config;
pub use container::{ContainerManager, ContainerState, ContainerStatus};
pub use error::{NodeError, Result};
pub use runtime::ContainerRuntime;

//! Nexus Node - Container runtime daemon for Nexus Panel
//!
//! Manages game server containers using Containerd, providing lifecycle management,
//! resource monitoring, and gRPC control API.
//!
//! # Enterprise Features
//!
//! This crate includes enterprise-grade features for production deployments:
//!
//! - **Authentication**: API key and JWT authentication middleware
//! - **Rate Limiting**: Configurable rate limiting with burst support
//! - **TLS/mTLS**: Secure communications with mutual TLS support
//! - **Audit Logging**: Comprehensive audit trail for compliance
//! - **Circuit Breakers**: Resilience patterns for cascading failure prevention
//! - **Request Tracing**: Distributed tracing with correlation IDs
//! - **Input Validation**: Security-focused input validation
//! - **Graceful Degradation**: Load shedding and feature flags
//! - **Hot Reload**: Configuration updates without restart

// Core modules
pub mod config;
pub mod container;
pub mod error;
pub mod grpc;
pub mod health;
pub mod metrics;
pub mod metrics_server;
pub mod runtime;
pub mod secrets;

// Enterprise modules
pub mod audit;
pub mod auth;
pub mod circuit_breaker;
pub mod config_reload;
pub mod graceful;
pub mod rate_limit;
pub mod tls;
pub mod tracing_middleware;
pub mod validation;

// Core re-exports
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
pub use secrets::{SecretsManager, SecretResolver, EnvSecretsManager, MemorySecretsManager};

// Enterprise re-exports
pub use audit::{AuditConfig, AuditEvent, AuditEventType, AuditLogger};
pub use auth::{AuthConfig, AuthInterceptor, Claims, Identity};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerRegistry, CircuitState};
pub use config_reload::{ConfigHolder, ConfigWatcher, EnvConfig, ReloadableConfig};
pub use graceful::{Bulkhead, FeatureFlags, GracefulShutdown, LoadShedder, LoadShedderConfig};
pub use rate_limit::{RateLimitConfig, RateLimiter_};
pub use tls::{TlsConfig, TlsError};
pub use tracing_middleware::{RequestContext, TracingConfig};
pub use validation::{ValidationError, Validator};

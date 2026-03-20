use nexus_node::{
    ContainerManager, ContainerdRuntime, HealthChecker, NodeServiceImpl, start_metrics_server,
    BackupManager, ScheduleManager,
    // Enterprise imports
    AuthConfig, AuditConfig, AuditLogger, AuditEvent, AuditEventType,
    RateLimitConfig, TlsConfig, TracingConfig, GracefulShutdown,
    CircuitBreakerConfig, CircuitBreakerRegistry, Validator,
    auth::layer::AuthLayer, rate_limit::layer::RateLimitLayer,
    tracing_middleware::layer::TracingLayer,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::transport::{Server, Identity, Certificate, ServerTlsConfig};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing with JSON support for production
    let json_logs = std::env::var("LOG_FORMAT")
        .map(|f| f.to_lowercase() == "json")
        .unwrap_or(false);

    if json_logs {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "nexus_node=info,tower_http=debug".into()),
            )
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "nexus_node=info,tower_http=debug".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

    info!("Starting Nexus Node v{} (Enterprise Edition)", env!("CARGO_PKG_VERSION"));

    // Configuration from environment variables
    let grpc_bind = std::env::var("GRPC_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let metrics_bind = std::env::var("METRICS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:9090".to_string());
    let containerd_socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());
    let containerd_namespace = std::env::var("CONTAINERD_NAMESPACE")
        .unwrap_or_else(|_| "nexus-panel".to_string());
    let data_dir = std::env::var("DATA_DIR")
        .unwrap_or_else(|_| "/var/lib/nexus-node".to_string());
    let node_id = std::env::var("NODE_ID")
        .unwrap_or_else(|_| hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "node-1".to_string())
        );

    // Health check configuration
    let min_disk_space = std::env::var("MIN_DISK_SPACE_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024 * 1024 * 1024); // 1GB default
    let min_memory = std::env::var("MIN_MEMORY_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512 * 1024 * 1024); // 512MB default

    // Enterprise feature configurations
    let auth_config = AuthConfig::from_env();
    let rate_limit_config = RateLimitConfig::from_env();
    let tls_config = TlsConfig::from_env().unwrap_or_default();
    let tracing_config = TracingConfig::from_env();
    let audit_config = AuditConfig::from_env();
    let circuit_breaker_config = CircuitBreakerConfig::from_env();

    info!("Configuration:");
    info!("  gRPC bind: {}", grpc_bind);
    info!("  Metrics bind: {}", metrics_bind);
    info!("  Containerd socket: {}", containerd_socket);
    info!("  Containerd namespace: {}", containerd_namespace);
    info!("  Data directory: {}", data_dir);
    info!("  Node ID: {}", node_id);
    info!("Enterprise Features:");
    info!("  Authentication: {}", if auth_config.enabled { "enabled" } else { "disabled" });
    info!("  Rate Limiting: {}", if rate_limit_config.enabled { "enabled" } else { "disabled" });
    info!("  TLS: {}", if tls_config.enabled { "enabled" } else { "disabled" });
    info!("  Audit Logging: {}", if audit_config.enabled { "enabled" } else { "disabled" });

    // Create data directory if it doesn't exist
    std::fs::create_dir_all(&data_dir)?;

    // Initialize Containerd runtime
    info!("Connecting to Containerd...");
    let runtime = Arc::new(ContainerdRuntime::new(
        containerd_socket.clone(),
        containerd_namespace.clone(),
    ));

    let use_mock_runtime = match runtime.connect().await {
        Ok(_) => {
            info!("Successfully connected to Containerd");
            false
        }
        Err(e) => {
            error!("Failed to connect to Containerd: {}", e);
            warn!("Falling back to mock runtime for development");
            // In production, you might want to exit here
            // For development, we'll use mock runtime
            true
        }
    };

    let runtime: Arc<dyn nexus_node::ContainerRuntime> = if use_mock_runtime {
        use nexus_node::runtime::mock::MockRuntime;
        Arc::new(MockRuntime::new())
    } else {
        runtime
    };

    // Create metrics instance
    let metrics = Arc::new(nexus_node::Metrics::new()?);

    // Create container manager with metrics
    let manager = Arc::new(ContainerManager::with_runtime_and_metrics(
        runtime,
        PathBuf::from(data_dir.clone()),
        metrics.clone(),
    ));

    // Create health checker
    let health_checker = Arc::new(RwLock::new(HealthChecker::new(
        containerd_socket,
        data_dir.clone(),
        min_disk_space,
        min_memory,
    )));

    // Initialize enterprise components
    let graceful_shutdown = Arc::new(GracefulShutdown::new());
    let circuit_breakers = Arc::new(CircuitBreakerRegistry::new(circuit_breaker_config));
    let validator = Arc::new(Validator::default());

    // Initialize audit logger
    let audit_logger = if audit_config.enabled {
        Some(Arc::new(AuditLogger::new(audit_config.clone()).await?))
    } else {
        None
    };

    // Log startup audit event
    if let Some(ref logger) = audit_logger {
        logger.log(
            AuditEvent::new(AuditEventType::NodeStarted, "node_startup")
                .with_node_id(&node_id)
                .success()
        ).await;
    }

    // Initialize backup manager
    let backup_manager = Arc::new(BackupManager::new(&PathBuf::from(data_dir.clone())));
    backup_manager.init().await?;

    // Initialize schedule manager
    let schedule_manager = Arc::new(ScheduleManager::new());

    // Create gRPC service
    let node_service = NodeServiceImpl::new(
        manager.clone(),
        node_id.clone(),
        health_checker.clone(),
        backup_manager.clone(),
        schedule_manager.clone(),
    ).into_server();

    // Parse bind addresses
    let grpc_addr = grpc_bind.parse()?;
    let metrics_addr = metrics_bind.clone();

    info!("Starting gRPC server on {}", grpc_addr);
    info!("Starting metrics server on {}", metrics_addr);

    // Spawn metrics server
    let metrics_server_handle = {
        let metrics = metrics.clone();
        tokio::spawn(async move {
            if let Err(e) = start_metrics_server(metrics, metrics_addr).await {
                error!("Metrics server error: {}", e);
            }
        })
    };

    // Spawn periodic health check updates
    let health_check_handle = {
        let health_checker = health_checker.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let mut checker = health_checker.write().await;
                let result = checker.check().await;

                // Update metrics based on health status
                if result.status == nexus_node::HealthStatus::Unhealthy {
                    warn!("Health check failed: {:?}", result.checks);
                }
            }
        })
    };

    // Spawn periodic metrics updates
    let metrics_update_handle = {
        let manager = manager.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                // Update container count metrics
                let containers = manager.list_containers().await;
                let total = containers.len();
                let running = containers.iter().filter(|c| c.status.is_running()).count();

                let mut by_state = std::collections::HashMap::new();
                for container in &containers {
                    let state_str = match container.status {
                        nexus_node::ContainerStatus::Created => "created",
                        nexus_node::ContainerStatus::Running => "running",
                        nexus_node::ContainerStatus::Stopped => "stopped",
                        nexus_node::ContainerStatus::Paused => "paused",
                        nexus_node::ContainerStatus::Failed => "failed",
                        nexus_node::ContainerStatus::Suspended => "suspended",
                    };
                    *by_state.entry(state_str).or_insert(0) += 1;
                }

                let by_state_vec: Vec<(&str, usize)> = by_state.iter()
                    .map(|(k, v)| (*k, *v))
                    .collect();

                metrics.update_container_counts(total, running, &by_state_vec);
            }
        })
    };

    // Build gRPC server with enterprise middleware layers
    // Note: Layers are applied bottom-up, so auth is checked first, then rate limiting, then tracing
    let shutdown = graceful_shutdown.clone();

    let mut server_builder = Server::builder();

    // Wire TLS if enabled
    if tls_config.enabled {
        let cert_path = tls_config.cert_path.as_ref()
            .expect("TLS_CERT_PATH required when TLS_ENABLED=true");
        let key_path = tls_config.key_path.as_ref()
            .expect("TLS_KEY_PATH required when TLS_ENABLED=true");

        let cert_pem = std::fs::read(cert_path)?;
        let key_pem = std::fs::read(key_path)?;
        let identity = Identity::from_pem(cert_pem, key_pem);

        let mut tls = ServerTlsConfig::new().identity(identity);

        if let Some(ca_path) = &tls_config.ca_cert_path {
            let ca_pem = std::fs::read(ca_path)?;
            tls = tls.client_ca_root(Certificate::from_pem(ca_pem));
            info!("TLS enabled with mTLS (client certificate verification)");
        } else {
            info!("TLS enabled (server-side only)");
        }

        server_builder = server_builder.tls_config(tls)?;
    }

    let grpc_server = server_builder
        .layer(TracingLayer::new(tracing_config))
        .layer(RateLimitLayer::new(rate_limit_config))
        .layer(AuthLayer::new(auth_config))
        .add_service(node_service)
        .serve_with_shutdown(grpc_addr, async move {
            shutdown.handle_signal().await;
        });

    // Wait for shutdown
    tokio::select! {
        result = grpc_server => {
            if let Err(e) = result {
                error!("gRPC server error: {}", e);
            }
        }
        _ = metrics_server_handle => {
            warn!("Metrics server stopped");
        }
        _ = health_check_handle => {
            warn!("Health check task stopped");
        }
        _ = metrics_update_handle => {
            warn!("Metrics update task stopped");
        }
    }

    // Log shutdown audit event
    if let Some(ref logger) = audit_logger {
        logger.log(
            AuditEvent::new(AuditEventType::NodeStopped, "node_shutdown")
                .with_node_id(&node_id)
                .success()
        ).await;
    }

    info!("Nexus Node shutting down");

    Ok(())
}

use nexus_node::{
    auth::layer::AuthLayer,
    rate_limit::layer::RateLimitLayer,
    start_metrics_server,
    tracing_middleware::layer::TracingLayer,
    AuditConfig,
    AuditEvent,
    AuditEventType,
    AuditLogger,
    // Enterprise imports
    AuthConfig,
    BackupManager,
    CircuitBreakerConfig,
    CircuitBreakerRegistry,
    ContainerManager,
    ContainerdRuntime,
    GracefulShutdown,
    HealthChecker,
    NodeServiceImpl,
    RateLimitConfig,
    ScheduleManager,
    TlsConfig,
    TracingConfig,
    Validator,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing with JSON support for production
    let json_logs =
        std::env::var("LOG_FORMAT").map(|f| f.to_lowercase() == "json").unwrap_or(false);

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

    info!(
        "Starting Nexus Node v{} (Enterprise Edition)",
        env!("CARGO_PKG_VERSION")
    );

    // Configuration from environment variables
    // All services default to loopback. Exposing the panel or gRPC API to a
    // network is a deliberate act: set the corresponding *_BIND env var (and
    // make sure authentication is configured first).
    let grpc_bind = std::env::var("GRPC_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let metrics_bind =
        std::env::var("METRICS_BIND").unwrap_or_else(|_| "127.0.0.1:9090".to_string());
    let web_bind = std::env::var("WEB_BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let containerd_socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());
    let containerd_namespace =
        std::env::var("CONTAINERD_NAMESPACE").unwrap_or_else(|_| "nexus-panel".to_string());
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "/var/lib/nexus-node".to_string());
    let node_id = std::env::var("NODE_ID").unwrap_or_else(|_| {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "node-1".to_string())
    });

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
    info!(
        "  Authentication: {}",
        if auth_config.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!(
        "  Rate Limiting: {}",
        if rate_limit_config.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!(
        "  TLS: {}",
        if tls_config.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!(
        "  Audit Logging: {}",
        if audit_config.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );

    // Create data directory if it doesn't exist
    std::fs::create_dir_all(&data_dir)?;

    // Initialize Containerd runtime
    info!("Connecting to Containerd...");
    let runtime = Arc::new(ContainerdRuntime::new(
        containerd_socket.clone(),
        containerd_namespace.clone(),
    ));

    // The in-memory mock runtime pretends to run containers and exists ONLY
    // for development/testing. In normal operation a containerd connection
    // failure is fatal — silently switching to a fake runtime would make the
    // panel report servers as "running" when nothing is actually running.
    let dev_mode = std::env::var("NEXUS_DEV_MODE")
        .map(|v| {
            let v = v.to_lowercase();
            v == "true" || v == "1"
        })
        .unwrap_or(false);

    let runtime: Arc<dyn nexus_node::ContainerRuntime> = match runtime.connect().await {
        Ok(_) => {
            info!("Successfully connected to Containerd");
            runtime
        }
        Err(e) if dev_mode => {
            error!("Failed to connect to Containerd: {}", e);
            warn!(
                "NEXUS_DEV_MODE is set — using the in-memory MOCK runtime. \
                 Containers are NOT real; never use this in production."
            );
            use nexus_node::runtime::mock::MockRuntime;
            Arc::new(MockRuntime::new())
        }
        Err(e) => {
            error!(
                "Failed to connect to Containerd at {}: {}",
                containerd_socket, e
            );
            error!(
                "Containerd is required. Ensure it is running (or fix CONTAINERD_SOCKET), \
                 or set NEXUS_DEV_MODE=true to run with the mock runtime for development."
            );
            return Err(format!("containerd connection failed: {}", e).into());
        }
    };

    // Create metrics instance
    let metrics = Arc::new(nexus_node::Metrics::new()?);

    // Create container manager with metrics
    let manager = Arc::new(ContainerManager::with_runtime_and_metrics(
        runtime,
        PathBuf::from(data_dir.clone()),
        metrics.clone(),
    ));

    // Restore previously-tracked containers from disk and reconcile them
    // against the runtime, so a node restart does not lose the server list.
    let restored = manager.restore().await;
    if restored > 0 {
        info!(
            "Reconciled {} existing container(s) after restart",
            restored
        );
    }

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
    info!(
        "  Circuit Breakers: {}",
        if circuit_breakers.all_stats().is_empty() {
            "enabled (no active breakers)"
        } else {
            "enabled"
        }
    );

    // Initialize audit logger
    let audit_logger = if audit_config.enabled {
        Some(Arc::new(AuditLogger::new(audit_config.clone()).await?))
    } else {
        None
    };

    // Log startup audit event
    if let Some(ref logger) = audit_logger {
        logger
            .log(
                AuditEvent::new(AuditEventType::NodeStarted, "node_startup")
                    .with_node_id(&node_id)
                    .success(),
            )
            .await;
    }

    // Initialize backup manager
    let backup_manager = Arc::new(BackupManager::new(&PathBuf::from(data_dir.clone())));
    backup_manager.init().await?;

    // Initialize schedule manager (persists schedules under DATA_DIR).
    let schedule_manager = Arc::new(ScheduleManager::with_data_dir(PathBuf::from(
        data_dir.clone(),
    )));
    let restored_schedules = schedule_manager.restore().await;
    if restored_schedules > 0 {
        info!("Restored {} schedule(s) after restart", restored_schedules);
    }

    // Start the background schedule runner with a callback that dispatches
    // tasks to the container manager / backup manager for real.
    schedule_manager
        .start_runner(nexus_node::schedule::dispatch_callback(
            manager.clone(),
            backup_manager.clone(),
        ))
        .await;

    // Create gRPC service with enterprise components
    let node_service = NodeServiceImpl::with_enterprise(
        manager.clone(),
        node_id.clone(),
        health_checker.clone(),
        backup_manager.clone(),
        schedule_manager.clone(),
        circuit_breakers,
        validator,
    )
    .into_server();

    // Parse bind addresses
    let grpc_addr = grpc_bind.parse()?;
    let metrics_addr = metrics_bind.clone();

    info!("Starting gRPC server on {}", grpc_addr);
    info!("Starting metrics server on {}", metrics_addr);
    info!("Starting web panel on {}", web_bind);

    // Spawn metrics server
    let metrics_server_handle = {
        let metrics = metrics.clone();
        tokio::spawn(async move {
            if let Err(e) = start_metrics_server(metrics, metrics_addr).await {
                error!("Metrics server error: {}", e);
            }
        })
    };

    // Initialize mod marketplace
    let marketplace = {
        use nexus_marketplace::adapters::{CodeflingAdapter, LoneDesignAdapter, UmodAdapter};
        let mut mgr = nexus_marketplace::MarketplaceManager::new();
        mgr.register_adapter(UmodAdapter::new());
        mgr.register_adapter(CodeflingAdapter::new());
        mgr.register_adapter(LoneDesignAdapter::new());
        Arc::new(mgr)
    };

    // Web panel authentication (shares AUTH_ENABLED / AUTH_PASSWORD /
    // AUTH_API_KEYS with the gRPC auth stack).
    let web_auth_config = nexus_node::web::auth::WebAuthConfig::from_env();
    info!(
        "  Web Panel Auth: {}",
        if web_auth_config.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    if web_auth_config.enabled && !web_auth_config.has_credentials() {
        warn!(
            "AUTH_ENABLED=true but neither AUTH_PASSWORD nor AUTH_API_KEYS is set — \
             the web panel will reject all API requests until a credential is configured"
        );
    }
    let web_sessions = Arc::new(nexus_node::web::auth::SessionStore::new(
        web_auth_config.session_ttl,
    ));
    let web_auth_config = Arc::new(web_auth_config);

    // Spawn web panel server
    let web_server_handle = {
        let web_state = nexus_node::web::AppState {
            manager: manager.clone(),
            backup_manager: backup_manager.clone(),
            schedule_manager: schedule_manager.clone(),
            health_checker: health_checker.clone(),
            metrics: metrics.clone(),
            marketplace: marketplace.clone(),
            node_id: node_id.clone(),
            data_dir: data_dir.clone(),
            start_time: std::time::SystemTime::now(),
            auth: web_auth_config.clone(),
            sessions: web_sessions.clone(),
            update_jobs: Arc::new(nexus_node::update::UpdateJobStore::new()),
        };
        tokio::spawn(async move {
            if let Err(e) = nexus_node::start_web_server(web_state, web_bind).await {
                error!("Web panel server error: {}", e);
            }
        })
    };

    // Spawn periodic health check updates
    let health_check_handle = {
        let health_checker = health_checker.clone();
        let _metrics = metrics.clone();
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

                let by_state_vec: Vec<(&str, usize)> =
                    by_state.iter().map(|(k, v)| (*k, *v)).collect();

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
        let cert_path = tls_config
            .cert_path
            .as_ref()
            .ok_or("TLS_CERT_PATH is required when TLS_ENABLED=true")?;
        let key_path = tls_config
            .key_path
            .as_ref()
            .ok_or("TLS_KEY_PATH is required when TLS_ENABLED=true")?;

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
        _ = web_server_handle => {
            warn!("Web panel server stopped");
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
        logger
            .log(
                AuditEvent::new(AuditEventType::NodeStopped, "node_shutdown")
                    .with_node_id(&node_id)
                    .success(),
            )
            .await;
    }

    info!("Nexus Node shutting down");

    Ok(())
}

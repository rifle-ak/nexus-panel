use nexus_node::{ContainerManager, ContainerdRuntime, NodeServiceImpl};
use std::path::PathBuf;
use std::sync::Arc;
use tonic::transport::Server;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nexus_node=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting Nexus Node v{}", env!("CARGO_PKG_VERSION"));

    // Configuration (TODO: Load from file/env)
    let grpc_bind = std::env::var("GRPC_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string());
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

    info!("Configuration:");
    info!("  gRPC bind: {}", grpc_bind);
    info!("  Containerd socket: {}", containerd_socket);
    info!("  Containerd namespace: {}", containerd_namespace);
    info!("  Data directory: {}", data_dir);
    info!("  Node ID: {}", node_id);

    // Create data directory if it doesn't exist
    std::fs::create_dir_all(&data_dir)?;

    // Initialize Containerd runtime
    info!("Connecting to Containerd...");
    let runtime = Arc::new(ContainerdRuntime::new(
        containerd_socket.clone(),
        containerd_namespace.clone(),
    ));

    match runtime.connect().await {
        Ok(_) => info!("Successfully connected to Containerd"),
        Err(e) => {
            error!("Failed to connect to Containerd: {}", e);
            error!("Falling back to mock runtime for development");
            // For development, we could fall back to mock runtime
            // In production, we should exit here
            return Err(format!("Containerd connection failed: {}", e).into());
        }
    }

    // Create container manager
    let manager = Arc::new(ContainerManager::with_runtime(
        runtime,
        PathBuf::from(data_dir),
    ));

    // Create gRPC service
    let node_service = NodeServiceImpl::new(manager.clone(), node_id).into_server();

    // Parse bind address
    let addr = grpc_bind.parse()?;

    info!("Starting gRPC server on {}", addr);

    // Start gRPC server
    Server::builder()
        .add_service(node_service)
        .serve(addr)
        .await?;

    Ok(())
}

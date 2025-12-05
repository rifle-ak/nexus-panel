use nexus_node::{ContainerManager, MockRuntime};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== Log Streaming Test ===\n");

    // Create temporary directory
    let temp_dir = TempDir::new()?;

    // Create container manager with mock runtime
    let runtime = Arc::new(MockRuntime::new());
    let manager = ContainerManager::with_runtime(runtime, temp_dir.path().to_path_buf());

    // Create a test config
    let config_yaml = r#"
metadata:
  id: test-server
  name: Test Log Streaming Server
  game: minecraft
  version: 1.0.0
  author: test@example.com

container:
  image: "itzg/minecraft-server:latest"
  environment: {}

resources:
  cpu:
    min: 1000
    max: 2000
    shares: 1024
  memory:
    min: 1Gi
    max: 2Gi
    swap: 512Mi
  disk:
    min: 5Gi
    io_priority: normal

startup:
  command: "java"
  args:
    - "-jar"
    - "server.jar"
  working_dir: /home/container
  lifecycle:
    pre_start: []
    post_start: []
    pre_stop: []

networking:
  ports:
    - name: game
      internal: "25565"
      protocol: tcp
      required: true

variables: []

security:
  capabilities:
    add: []
    drop: []
  firewall_rules: []
"#;

    let config = nexus_config::GameConfig::from_yaml(config_yaml)?;

    // Create container
    println!("Creating container...");
    let container_id = manager
        .create_container(&config, Some("test-logs".to_string()))
        .await?;
    println!("✓ Container created: {}\n", container_id);

    // Start container
    println!("Starting container...");
    manager.start_container(&container_id).await?;
    println!("✓ Container started\n");

    // Attach to console and stream logs
    println!("Streaming logs:");
    println!("─────────────────────────────────────────────────────\n");

    let mut console = manager.attach_console(&container_id).await?;

    let mut line_count = 0;
    while let Some(line) = console.read_line().await? {
        println!("{}", line);
        line_count += 1;
    }

    println!("\n─────────────────────────────────────────────────────");
    println!("✓ Received {} log lines", line_count);

    // Clean up
    println!("\nCleaning up...");
    manager.stop_container(&container_id, Some(5)).await?;
    manager.delete_container(&container_id, false).await?;
    println!("✓ Cleanup complete");

    Ok(())
}

//! Integration test: Load real converted eggs and create containers
//!
//! This tests the complete flow from loading a Nexus YAML config
//! to creating a container from it.

use nexus_node::{ContainerManager, load_config};
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    println!("=================================================");
    println!("Nexus Node - Real Config Integration Test");
    println!("=================================================\n");

    // Create temp dir for container data
    let temp_dir = TempDir::new()?;
    let manager = ContainerManager::new(temp_dir.path().to_path_buf());

    // Test configs to load
    let test_configs = vec![
        ("Minecraft Paper", "converted_eggs/egg-paper.yaml"),
        ("Valheim", "converted_eggs/egg-valheim.yaml"),
        ("Terraria", "converted_eggs/egg-terraria-vanilla.yaml"),
    ];

    let mut success_count = 0;
    let mut fail_count = 0;

    for (name, path) in test_configs {
        print!("Testing {}: ", name);

        let config_path = PathBuf::from(path);
        if !config_path.exists() {
            println!("⚠️  SKIPPED (file not found)");
            continue;
        }

        // Load config
        match load_config(&config_path) {
            Ok(config) => {
                println!("✓ Config loaded");
                println!("  - Image: {}", config.container.image);
                println!("  - Ports: {}", config.networking.ports.len());
                println!("  - Variables: {}", config.variables.len());

                // Create container
                match manager.create_container(&config, None).await {
                    Ok(container_id) => {
                        println!("  ✓ Container created: {}", container_id);

                        // Get state
                        if let Ok(state) = manager.get_state(&container_id).await {
                            println!("  ✓ State: {:?}", state.status);
                        }

                        success_count += 1;
                    }
                    Err(e) => {
                        println!("  ✗ Container creation failed: {}", e);
                        fail_count += 1;
                    }
                }
            }
            Err(e) => {
                println!("✗ Config load failed: {}", e);
                fail_count += 1;
            }
        }
        println!();
    }

    println!("=================================================");
    println!("RESULTS");
    println!("=================================================");
    println!("✓ Success: {}", success_count);
    println!("✗ Failed: {}", fail_count);
    println!();

    if fail_count == 0 {
        println!("🎉 All tests passed!");
        Ok(())
    } else {
        anyhow::bail!("{} tests failed", fail_count);
    }
}

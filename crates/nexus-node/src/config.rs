use crate::error::{NodeError, Result};
use nexus_config::GameConfig;
use std::path::Path;
use tracing::{debug, info};

/// Loads a Nexus game config from a YAML file
pub fn load_config(path: impl AsRef<Path>) -> Result<GameConfig> {
    let path = path.as_ref();
    info!("Loading config from: {}", path.display());

    let content = std::fs::read_to_string(path).map_err(|e| NodeError::ConfigLoadError {
        path: path.display().to_string(),
        source: e.into(),
    })?;

    let config = GameConfig::from_yaml(&content).map_err(|e| NodeError::ConfigLoadError {
        path: path.display().to_string(),
        source: e,
    })?;

    debug!(
        "Loaded config for: {} ({})",
        config.metadata.name, config.metadata.game
    );

    // Validate config
    validate_config(&config)?;

    Ok(config)
}

/// Validates a game config for runtime requirements
fn validate_config(config: &GameConfig) -> Result<()> {
    // Check that we have a Docker image
    if config.container.image.is_empty() {
        return Err(NodeError::InvalidConfig {
            reason: "Container image is required".to_string(),
        });
    }

    // Check that we have a startup command
    if config.startup.command.is_empty() {
        return Err(NodeError::InvalidConfig {
            reason: "Startup command is required".to_string(),
        });
    }

    // Check that port bindings are valid (skip template values)
    for port in &config.networking.ports {
        // Skip validation for template values like {{SERVER_PORT}}
        if !port.internal.contains("{{") && !port.internal.is_empty() {
            if let Ok(port_num) = port.internal.parse::<u16>() {
                if port_num == 0 {
                    return Err(NodeError::InvalidConfig {
                        reason: "Invalid port binding: internal port cannot be 0".to_string(),
                    });
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_valid_config() {
        let yaml = r#"
metadata:
  id: test-server
  name: Test Server
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

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();
        file.flush().unwrap();

        let config = load_config(file.path()).unwrap();
        assert_eq!(config.metadata.name, "Test Server");
        assert_eq!(config.metadata.game, "minecraft");
        assert_eq!(config.metadata.id, "test-server");
    }

    #[test]
    fn test_invalid_config_no_image() {
        let yaml = r#"
metadata:
  id: test-server
  name: Test Server
  game: minecraft
  version: 1.0.0
  author: test@example.com

container:
  image: ""
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
  args: []
  working_dir: /home/container
  lifecycle:
    pre_start: []
    post_start: []
    pre_stop: []

networking:
  ports: []

variables: []

security:
  capabilities:
    add: []
    drop: []
  firewall_rules: []
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_config(file.path());
        assert!(result.is_err());
    }
}

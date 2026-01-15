use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pre-compiled regex for memory format validation (e.g., 512Mi, 4Gi, 1Ti)
static MEMORY_FORMAT_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\d+(\.\d+)?(Mi|Gi|Ti)$").expect("Invalid memory format regex"));

pub mod errors;
pub mod diagnostics;
pub use errors::NexusPanelError;
pub use diagnostics::Diagnostics;

/// Native game server configuration format
/// This is our superior alternative to Pterodactyl eggs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameConfig {
    pub metadata: Metadata,
    pub container: Container,
    pub resources: Resources,
    pub startup: Startup,
    pub variables: Vec<Variable>,
    pub networking: Networking,
    pub security: Security,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitoring: Option<Monitoring>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backups: Option<Backups>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub game: String,
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resources {
    pub cpu: CpuResources,
    pub memory: MemoryResources,
    pub disk: DiskResources,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuResources {
    /// Minimum CPU in millicores (1000 = 1 core)
    pub min: u32,
    /// Maximum CPU in millicores
    pub max: u32,
    /// CPU shares for scheduling priority
    #[serde(default = "default_cpu_shares")]
    pub shares: u32,
}

fn default_cpu_shares() -> u32 {
    1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResources {
    /// Minimum memory (e.g., "2Gi", "512Mi")
    pub min: String,
    /// Maximum memory
    pub max: String,
    /// Swap memory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskResources {
    /// Minimum disk space
    pub min: String,
    /// I/O priority: low, normal, high
    #[serde(default = "default_io_priority")]
    pub io_priority: String,
}

fn default_io_priority() -> String {
    "normal".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Startup {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub working_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<Lifecycle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lifecycle {
    #[serde(default)]
    pub pre_start: Vec<LifecycleAction>,
    #[serde(default)]
    pub post_start: Vec<LifecycleAction>,
    #[serde(default)]
    pub pre_stop: Vec<LifecycleAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LifecycleAction {
    Download {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
    },
    Execute {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
    },
    HealthCheck {
        endpoint: String,
        timeout: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub description: String,
    pub default: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub user_editable: bool,
    #[serde(default)]
    pub user_viewable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<ValidationRule>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValidationRule {
    Port {
        range: (u16, u16),
    },
    Regex {
        pattern: String,
    },
    Enum {
        values: Vec<String>,
    },
    Numeric {
        min: Option<i64>,
        max: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Networking {
    pub ports: Vec<Port>,
    #[serde(default)]
    pub dns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Port {
    pub name: String,
    pub internal: String, // Can be template like "{{SERVER_PORT}}"
    pub protocol: Protocol,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firewall_default: Option<FirewallDefault>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FirewallDefault {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Security {
    pub capabilities: Capabilities,
    #[serde(default)]
    pub read_only_root: bool,
    #[serde(default = "default_true")]
    pub no_new_privileges: bool,
    #[serde(default = "default_seccomp")]
    pub seccomp_profile: String,
    #[serde(default)]
    pub firewall_rules: Vec<FirewallRule>,
}

fn default_true() -> bool {
    true
}

fn default_seccomp() -> String {
    "runtime/default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub drop: Vec<String>,
    #[serde(default)]
    pub add: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FirewallRule {
    ConnectionRate {
        name: String,
        limit: String, // e.g., "100/s"
        action: FirewallAction,
    },
    PacketSize {
        name: String,
        max_size: u32,
        action: FirewallAction,
    },
    AllowCidr {
        name: String,
        cidr: String,
    },
    BlockCidr {
        name: String,
        cidr: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FirewallAction {
    Allow,
    Drop,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Monitoring {
    pub health_check: HealthCheck,
    #[serde(default)]
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HealthCheck {
    Tcp {
        port: String,
        interval: String,
        timeout: String,
        failure_threshold: u32,
    },
    Http {
        path: String,
        port: String,
        interval: String,
        timeout: String,
        failure_threshold: u32,
    },
    Rcon {
        command: String,
        interval: String,
        timeout: String,
        failure_threshold: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Metric {
    Rcon {
        name: String,
        command: String,
        parse: String, // Regex to extract value
        interval: String,
    },
    Http {
        name: String,
        url: String,
        parse: String,
        interval: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backups {
    pub paths: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>, // Cron expression
}

impl GameConfig {
    /// Validate the configuration with detailed error messages
    pub fn validate(&self) -> Result<(), NexusPanelError> {
        let mut errors = Vec::new();

        // Validate resource values
        if self.resources.cpu.min > self.resources.cpu.max {
            errors.push(format!(
                "  • CPU min ({}) cannot be greater than max ({})",
                self.resources.cpu.min, self.resources.cpu.max
            ));
        }

        if self.resources.cpu.min < 100 {
            errors.push(format!(
                "  • CPU min ({}) is too low. Minimum recommended: 100 millicores (0.1 cores)",
                self.resources.cpu.min
            ));
        }

        // Validate memory format
        if !Self::is_valid_memory_format(&self.resources.memory.min) {
            errors.push(format!(
                "  • Invalid memory format for 'min': '{}'. Use format like '512Mi', '4Gi', '8Gi'",
                self.resources.memory.min
            ));
        }

        if !Self::is_valid_memory_format(&self.resources.memory.max) {
            errors.push(format!(
                "  • Invalid memory format for 'max': '{}'. Use format like '512Mi', '4Gi', '8Gi'",
                self.resources.memory.max
            ));
        }

        // Validate disk format
        if !Self::is_valid_memory_format(&self.resources.disk.min) {
            errors.push(format!(
                "  • Invalid disk format: '{}'. Use format like '10Gi', '50Gi', '100Gi'",
                self.resources.disk.min
            ));
        }

        // Validate variables
        for var in &self.variables {
            // Required variables must have a default unless they're user_editable
            // Note: This is now a warning handled elsewhere, not a blocking error
            // Some Pterodactyl eggs have this configuration legitimately

            // Validate rules if present
            if let Some(rules) = &var.rules {
                for rule in rules {
                    match rule {
                        ValidationRule::Port { range } => {
                            if range.0 >= range.1 {
                                errors.push(format!(
                                    "  • Variable '{}': invalid port range ({}-{})",
                                    var.name, range.0, range.1
                                ));
                            }
                        }
                        ValidationRule::Regex { pattern } => {
                            if regex::Regex::new(pattern).is_err() {
                                errors.push(format!(
                                    "  • Variable '{}': invalid regex pattern '{}'",
                                    var.name, pattern
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Validate ports
        for port in &self.networking.ports {
            if port.required && port.internal.is_empty() {
                errors.push(format!(
                    "  • Port '{}': required but has no internal port specified",
                    port.name
                ));
            }

            // Check for template variables
            if port.internal.contains("{{") && !port.internal.contains("}}") {
                errors.push(format!(
                    "  • Port '{}': malformed template variable in '{}'",
                    port.name, port.internal
                ));
            }
        }

        // Validate startup command
        if self.startup.command.is_empty() {
            errors.push("  • Startup command cannot be empty".to_string());
        }

        // Validate container image
        if self.container.image.is_empty() {
            errors.push("  • Container image cannot be empty".to_string());
        }
        // Note: Missing tag is a warning, not an error - handled separately in check_config_warnings

        // Return all errors if any
        if !errors.is_empty() {
            return Err(NexusPanelError::ValidationError {
                config_name: self.metadata.name.clone(),
                errors: errors.join("\n"),
            });
        }

        Ok(())
    }

    fn is_valid_memory_format(mem: &str) -> bool {
        // Valid formats: 512Mi, 4Gi, 8Gi, 1Ti
        // Uses pre-compiled regex for performance
        MEMORY_FORMAT_REGEX.is_match(mem)
    }

    /// Export to YAML
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// Load from YAML
    pub fn from_yaml(yaml: &str) -> anyhow::Result<Self> {
        let config: GameConfig = serde_yaml::from_str(yaml)?;
        config.validate()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_deserialize() {
        let config = GameConfig {
            metadata: Metadata {
                id: "rust-test".to_string(),
                name: "Rust Test Server".to_string(),
                version: "1.0.0".to_string(),
                game: "rust".to_string(),
                author: "test".to_string(),
                description: None,
                tags: None,
            },
            container: Container {
                image: "ghcr.io/test/rust:latest".to_string(),
                entrypoint: None,
                environment: HashMap::new(),
            },
            resources: Resources {
                cpu: CpuResources {
                    min: 2000,
                    max: 4000,
                    shares: 1024,
                },
                memory: MemoryResources {
                    min: "4Gi".to_string(),
                    max: "8Gi".to_string(),
                    swap: None,
                },
                disk: DiskResources {
                    min: "10Gi".to_string(),
                    io_priority: "normal".to_string(),
                },
            },
            startup: Startup {
                command: "./RustDedicated".to_string(),
                args: vec!["-batchmode".to_string()],
                working_dir: "/home/container".to_string(),
                lifecycle: None,
            },
            variables: vec![],
            networking: Networking {
                ports: vec![],
                dns: vec![],
            },
            security: Security {
                capabilities: Capabilities {
                    drop: vec!["ALL".to_string()],
                    add: vec![],
                },
                read_only_root: false,
                no_new_privileges: true,
                seccomp_profile: "runtime/default".to_string(),
                firewall_rules: vec![],
            },
            monitoring: None,
            backups: None,
        };

        let yaml = config.to_yaml().unwrap();
        let parsed = GameConfig::from_yaml(&yaml).unwrap();
        assert_eq!(config.metadata.id, parsed.metadata.id);
    }
}

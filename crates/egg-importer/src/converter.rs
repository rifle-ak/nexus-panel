use crate::pterodactyl::{EggVariable, PterodactylEgg};
use anyhow::{Context, Result};
use game_config::*;
use regex::Regex;
use std::collections::HashMap;

pub struct EggConverter {
    /// Security scanning enabled
    security_scan: bool,
    /// Add default firewall rules
    add_firewall_rules: bool,
}

impl Default for EggConverter {
    fn default() -> Self {
        Self {
            security_scan: true,
            add_firewall_rules: true,
        }
    }
}

impl EggConverter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn security_scan(mut self, enabled: bool) -> Self {
        self.security_scan = enabled;
        self
    }

    pub fn add_firewall_rules(mut self, enabled: bool) -> Self {
        self.add_firewall_rules = enabled;
        self
    }

    /// Convert Pterodactyl egg to native GameConfig
    pub fn convert(&self, egg: &PterodactylEgg) -> Result<GameConfig> {
        tracing::info!("Converting egg: {}", egg.name);

        // Security scan the egg first
        if self.security_scan {
            self.scan_egg_security(egg)?;
        }

        let config = GameConfig {
            metadata: self.convert_metadata(egg)?,
            container: self.convert_container(egg)?,
            resources: self.convert_resources(egg)?,
            startup: self.convert_startup(egg)?,
            variables: self.convert_variables(&egg.variables)?,
            networking: self.convert_networking(egg)?,
            security: self.convert_security(egg)?,
            monitoring: self.convert_monitoring(egg)?,
            backups: None, // Eggs don't have backup config
        };

        config.validate()?;
        Ok(config)
    }

    fn scan_egg_security(&self, egg: &PterodactylEgg) -> Result<()> {
        tracing::info!("Scanning egg for security issues");

        // Check for dangerous commands in startup
        let dangerous_patterns = vec![
            (r"rm\s+-rf\s+/", "Dangerous: recursive root deletion"),
            (r"chmod\s+777", "Warning: overly permissive permissions"),
            (r"curl.*\|\s*bash", "Dangerous: piping curl to bash"),
            (r"wget.*\|\s*bash", "Dangerous: piping wget to bash"),
            (r"eval\s+\$", "Warning: eval with variables"),
            (r"--privileged", "Dangerous: privileged container"),
        ];

        for (pattern, message) in dangerous_patterns {
            let re = Regex::new(pattern)?;
            if re.is_match(&egg.startup) {
                tracing::warn!("Security issue in startup command: {}", message);
            }
            if re.is_match(&egg.scripts.installation.script) {
                tracing::warn!("Security issue in installation script: {}", message);
            }
        }

        Ok(())
    }

    fn convert_metadata(&self, egg: &PterodactylEgg) -> Result<Metadata> {
        // Generate a clean ID from the name
        let id = egg
            .name
            .to_lowercase()
            .replace(" ", "-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect();

        Ok(Metadata {
            id,
            name: egg.name.clone(),
            version: egg.meta.version.clone(),
            game: self.detect_game_type(egg),
            author: egg.author.clone(),
            description: egg.description.clone(),
            tags: egg.features.clone(),
        })
    }

    fn detect_game_type(&self, egg: &PterodactylEgg) -> String {
        // Try to detect game type from name or description
        let searchable = format!(
            "{} {}",
            egg.name.to_lowercase(),
            egg.description.as_deref().unwrap_or("").to_lowercase()
        );

        if searchable.contains("minecraft") {
            "minecraft".to_string()
        } else if searchable.contains("rust") {
            "rust".to_string()
        } else if searchable.contains("ark") {
            "ark".to_string()
        } else if searchable.contains("valheim") {
            "valheim".to_string()
        } else if searchable.contains("terraria") {
            "terraria".to_string()
        } else if searchable.contains("csgo") || searchable.contains("counter-strike") {
            "csgo".to_string()
        } else if searchable.contains("satisfactory") {
            "satisfactory".to_string()
        } else if searchable.contains("palworld") {
            "palworld".to_string()
        } else {
            "generic".to_string()
        }
    }

    fn convert_container(&self, egg: &PterodactylEgg) -> Result<Container> {
        let image = egg
            .default_image()
            .context("No docker image found in egg")?;

        Ok(Container {
            image,
            entrypoint: None,
            environment: HashMap::new(),
        })
    }

    fn convert_resources(&self, egg: &PterodactylEgg) -> Result<Resources> {
        // Pterodactyl eggs don't specify resources, so we set sensible defaults
        // based on the game type
        let game = self.detect_game_type(egg);

        let (cpu_min, cpu_max, mem_min, mem_max, disk_min) = match game.as_str() {
            "rust" => (2000, 4000, "4Gi", "8Gi", "20Gi"),
            "ark" => (4000, 6000, "8Gi", "16Gi", "50Gi"),
            "minecraft" => (1000, 2000, "2Gi", "4Gi", "10Gi"),
            "valheim" => (2000, 4000, "4Gi", "8Gi", "10Gi"),
            _ => (1000, 2000, "1Gi", "2Gi", "5Gi"),
        };

        Ok(Resources {
            cpu: CpuResources {
                min: cpu_min,
                max: cpu_max,
                shares: 1024,
            },
            memory: MemoryResources {
                min: mem_min.to_string(),
                max: mem_max.to_string(),
                swap: Some("512Mi".to_string()),
            },
            disk: DiskResources {
                min: disk_min.to_string(),
                io_priority: "normal".to_string(),
            },
        })
    }

    fn convert_startup(&self, egg: &PterodactylEgg) -> Result<Startup> {
        // Parse startup command - eggs often have complex shell commands
        let (command, args) = self.parse_startup_command(&egg.startup)?;

        // Convert installation script to lifecycle hooks
        let lifecycle = if !egg.scripts.installation.script.is_empty() {
            Some(self.convert_installation_script(egg)?)
        } else {
            None
        };

        Ok(Startup {
            command,
            args,
            working_dir: "/home/container".to_string(),
            lifecycle,
        })
    }

    fn parse_startup_command(&self, startup: &str) -> Result<(String, Vec<String>)> {
        // Handle common patterns in egg startup commands
        let trimmed = startup.trim();

        // Simple case: just a command with args
        if !trimmed.contains("&&") && !trimmed.contains(";") && !trimmed.contains("|") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.is_empty() {
                anyhow::bail!("Empty startup command");
            }

            let command = parts[0].to_string();
            let args = parts[1..].iter().map(|s| s.to_string()).collect();
            return Ok((command, args));
        }

        // Complex case: wrap in shell script
        tracing::warn!("Complex startup command detected, wrapping in shell");
        Ok(("bash".to_string(), vec!["-c".to_string(), trimmed.to_string()]))
    }

    fn convert_installation_script(&self, egg: &PterodactylEgg) -> Result<Lifecycle> {
        // Convert Pterodactyl installation script to structured lifecycle hooks
        // This is a simplified conversion - real implementation would be more sophisticated

        let script = &egg.scripts.installation.script;
        let mut pre_start = Vec::new();

        // Look for common patterns like apt-get, downloads, etc.
        if script.contains("steamcmd") {
            pre_start.push(LifecycleAction::Execute {
                command: egg.scripts.installation.script.clone(),
                condition: Some("first_start".to_string()),
                timeout: Some("300s".to_string()),
            });
        }

        Ok(Lifecycle {
            pre_start,
            post_start: vec![],
            pre_stop: vec![self.convert_stop_command(&egg.config.stop)],
        })
    }

    fn convert_stop_command(&self, stop: &str) -> LifecycleAction {
        LifecycleAction::Execute {
            command: stop.to_string(),
            condition: None,
            timeout: Some("30s".to_string()),
        }
    }

    fn convert_variables(&self, variables: &[EggVariable]) -> Result<Vec<Variable>> {
        variables
            .iter()
            .map(|var| self.convert_variable(var))
            .collect()
    }

    fn convert_variable(&self, var: &EggVariable) -> Result<Variable> {
        let rules = self.parse_validation_rules(&var.rules)?;

        // Check if this is a secret (password, token, key, etc.)
        let secret = var.name.to_lowercase().contains("password")
            || var.name.to_lowercase().contains("token")
            || var.name.to_lowercase().contains("secret")
            || var.name.to_lowercase().contains("key");

        Ok(Variable {
            name: var.env_variable.clone(),
            description: var.description.clone(),
            default: var.default_value.clone(),
            required: var.rules.contains("required"),
            secret,
            user_editable: var.user_editable,
            user_viewable: var.user_viewable,
            rules: if rules.is_empty() { None } else { Some(rules) },
        })
    }

    fn parse_validation_rules(&self, rules: &str) -> Result<Vec<ValidationRule>> {
        let mut parsed_rules = Vec::new();

        // Parse Pterodactyl validation rules format
        // Common formats: "required|string|min:8", "required|numeric|between:1024,65535"
        for rule in rules.split('|') {
            let rule = rule.trim();

            if rule.starts_with("between:") {
                // Port range: "between:1024,65535"
                let range_str = rule.strip_prefix("between:").unwrap();
                let parts: Vec<&str> = range_str.split(',').collect();
                if parts.len() == 2 {
                    if let (Ok(min), Ok(max)) = (parts[0].parse::<u16>(), parts[1].parse::<u16>())
                    {
                        parsed_rules.push(ValidationRule::Port { range: (min, max) });
                    }
                }
            } else if rule.starts_with("regex:") {
                let pattern = rule.strip_prefix("regex:").unwrap();
                parsed_rules.push(ValidationRule::Regex {
                    pattern: pattern.to_string(),
                });
            } else if rule.starts_with("in:") {
                let values_str = rule.strip_prefix("in:").unwrap();
                let values: Vec<String> = values_str.split(',').map(|s| s.to_string()).collect();
                parsed_rules.push(ValidationRule::Enum { values });
            } else if rule.starts_with("min:") || rule.starts_with("max:") {
                // Numeric min/max
                // This is simplified - real implementation would handle both
                continue;
            }
        }

        Ok(parsed_rules)
    }

    fn convert_networking(&self, egg: &PterodactylEgg) -> Result<Networking> {
        let mut ports = Vec::new();

        // Extract ports from variables
        for var in &egg.variables {
            if var.name.to_lowercase().contains("port")
                || var.env_variable.to_lowercase().contains("port")
            {
                let protocol = if var.name.to_lowercase().contains("query")
                    || var.name.to_lowercase().contains("rcon")
                {
                    Protocol::Tcp
                } else {
                    // Most game servers use UDP for main port
                    Protocol::Udp
                };

                let firewall_default = if var.name.to_lowercase().contains("rcon") {
                    Some(FirewallDefault::Deny) // RCON should be restricted by default
                } else {
                    None
                };

                ports.push(Port {
                    name: var.name.clone(),
                    internal: format!("{{{{{}}}}}", var.env_variable),
                    protocol,
                    required: var.rules.contains("required"),
                    firewall_default,
                });
            }
        }

        Ok(Networking {
            ports,
            dns: vec!["1.1.1.1".to_string(), "1.0.0.1".to_string()],
        })
    }

    fn convert_security(&self, _egg: &PterodactylEgg) -> Result<Security> {
        let mut firewall_rules = Vec::new();

        // Add default DDoS protection rules if enabled
        if self.add_firewall_rules {
            firewall_rules.push(FirewallRule::ConnectionRate {
                name: "rate_limit_connections".to_string(),
                limit: "100/s".to_string(),
                action: FirewallAction::Drop,
            });

            firewall_rules.push(FirewallRule::PacketSize {
                name: "block_amplification".to_string(),
                max_size: 1500,
                action: FirewallAction::Drop,
            });
        }

        Ok(Security {
            capabilities: Capabilities {
                drop: vec!["ALL".to_string()],
                add: vec!["NET_BIND_SERVICE".to_string()],
            },
            read_only_root: false,
            no_new_privileges: true,
            seccomp_profile: "runtime/default".to_string(),
            firewall_rules,
        })
    }

    fn convert_monitoring(&self, egg: &PterodactylEgg) -> Result<Option<Monitoring>> {
        // Try to detect if server has RCON or query port for health checks
        let has_rcon = egg
            .variables
            .iter()
            .any(|v| v.name.to_lowercase().contains("rcon"));

        if has_rcon {
            Ok(Some(Monitoring {
                health_check: HealthCheck::Rcon {
                    command: "status".to_string(),
                    interval: "30s".to_string(),
                    timeout: "5s".to_string(),
                    failure_threshold: 3,
                },
                metrics: vec![],
            }))
        } else {
            // Try TCP health check on main port
            let main_port = egg
                .variables
                .iter()
                .find(|v| {
                    v.name.to_lowercase().contains("port") && !v.name.to_lowercase().contains("rcon")
                })
                .map(|v| format!("{{{{{}}}}}", v.env_variable));

            if let Some(port) = main_port {
                Ok(Some(Monitoring {
                    health_check: HealthCheck::Tcp {
                        port,
                        interval: "30s".to_string(),
                        timeout: "5s".to_string(),
                        failure_threshold: 3,
                    },
                    metrics: vec![],
                }))
            } else {
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_startup() {
        let converter = EggConverter::new();
        let (cmd, args) = converter
            .parse_startup_command("./server -port 25565")
            .unwrap();
        assert_eq!(cmd, "./server");
        assert_eq!(args, vec!["-port", "25565"]);
    }

    #[test]
    fn test_detect_game_type() {
        let converter = EggConverter::new();
        let mut egg = PterodactylEgg {
            comment: None,
            meta: crate::pterodactyl::Meta {
                version: "PTDL_v2".to_string(),
                update_url: None,
            },
            exported_at: "2024-01-01".to_string(),
            name: "Rust Dedicated Server".to_string(),
            author: "test".to_string(),
            description: None,
            features: None,
            docker_images: HashMap::new(),
            file_denylist: vec![],
            startup: "".to_string(),
            config: crate::pterodactyl::Config {
                files: HashMap::new(),
                startup: crate::pterodactyl::StartupConfig {
                    done: vec![],
                    user_interaction: vec![],
                },
                stop: "".to_string(),
                logs: crate::pterodactyl::LogsConfig {
                    custom: false,
                    location: "".to_string(),
                },
                file_denylist: vec![],
            },
            scripts: crate::pterodactyl::Scripts {
                installation: crate::pterodactyl::InstallationScript {
                    script: "".to_string(),
                    container: "".to_string(),
                    entrypoint: "".to_string(),
                },
            },
            variables: vec![],
        };

        assert_eq!(converter.detect_game_type(&egg), "rust");

        egg.name = "Minecraft Java Server".to_string();
        assert_eq!(converter.detect_game_type(&egg), "minecraft");
    }
}

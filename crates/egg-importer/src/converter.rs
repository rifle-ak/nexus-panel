use crate::pterodactyl::{EggVariable, PterodactylEgg};
use anyhow::Result;
use nexus_config::*;
use regex::Regex;
use std::collections::HashMap;

/// Where Pterodactyl mounts a server's directory while its installation
/// script runs. Egg scripts are written against this path.
const PTERODACTYL_INSTALL_DIR: &str = "/mnt/server";

/// Converts Pterodactyl eggs to Nexus Blueprints
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

    /// Convert Pterodactyl egg to Nexus Blueprint.
    ///
    /// Security findings are logged; use [`convert_with_report`](Self::convert_with_report)
    /// when the caller wants to surface them (e.g. in the panel's import UI).
    pub fn convert(&self, egg: &PterodactylEgg) -> Result<Blueprint> {
        let (blueprint, warnings) = self.convert_with_report(egg)?;
        for warning in &warnings {
            tracing::warn!("{}", warning);
        }
        Ok(blueprint)
    }

    /// Convert a Pterodactyl egg, also returning any security findings so the
    /// caller can show the operator what the egg does before they deploy it.
    pub fn convert_with_report(&self, egg: &PterodactylEgg) -> Result<(Blueprint, Vec<String>)> {
        tracing::info!("Converting egg to blueprint: {}", egg.name);

        // Security scan the egg first
        let warnings = if self.security_scan {
            self.scan_egg_security(egg)?
        } else {
            Vec::new()
        };

        let game_type = self.detect_game_type(egg);

        let blueprint = Blueprint {
            blueprint_version: "1.0".to_string(),
            metadata: self.convert_metadata(egg)?,
            container: self.convert_container(egg)?,
            resources: self.convert_resources(egg)?,
            startup: self.convert_startup(egg)?,
            variables: self.convert_variables(&egg.variables)?,
            networking: self.convert_networking(egg)?,
            security: self.convert_security(egg)?,
            monitoring: self.convert_monitoring(egg)?,
            backups: self.generate_backup_config(&game_type),
            performance: self.generate_performance_config(&game_type),
            scaling: None, // Eggs don't have scaling config
            mods: self.detect_mod_support(&game_type),
            install: self.convert_install(egg),
            updates: self.generate_update_config(egg),
            dependencies: None,
            clustering: None,
        };

        blueprint.validate()?;
        Ok((blueprint, warnings))
    }

    /// Generate backup configuration based on game type
    fn generate_backup_config(&self, game_type: &str) -> Option<Backups> {
        let (paths, exclude) = match game_type {
            "minecraft" => (
                vec![
                    "/world".to_string(),
                    "/world_nether".to_string(),
                    "/world_the_end".to_string(),
                    "/plugins".to_string(),
                ],
                vec![
                    "*.log".to_string(),
                    "/logs".to_string(),
                    "/cache".to_string(),
                ],
            ),
            "rust" => (
                vec!["/server".to_string(), "/oxide".to_string()],
                vec!["*.log".to_string(), "/oxide/logs".to_string()],
            ),
            "valheim" => (
                vec!["/saves".to_string(), "/BepInEx".to_string()],
                vec!["*.log".to_string()],
            ),
            _ => (
                vec!["/home/container".to_string()],
                vec!["*.log".to_string(), "*.tmp".to_string()],
            ),
        };

        Some(Backups {
            paths,
            exclude,
            schedule: None,
            retention: Some(5),
            pre_backup_command: None,
        })
    }

    /// Generate performance configuration based on game type
    fn generate_performance_config(&self, game_type: &str) -> Option<Performance> {
        match game_type {
            "minecraft" => Some(Performance {
                jvm: Some(JvmTuning {
                    gc: "g1gc".to_string(),
                    initial_heap: None,
                    max_heap: None,
                    flags: vec![],
                    aikar_flags: true,
                }),
                kernel: None,
                nice: Some(-5),
                io_class: Some("best-effort".to_string()),
                cpu_affinity: None,
                huge_pages: None,
            }),
            "rust" | "ark" => Some(Performance {
                jvm: None,
                kernel: Some(KernelTuning {
                    sysctl: [
                        ("net.core.rmem_max".to_string(), "26214400".to_string()),
                        ("net.core.wmem_max".to_string(), "26214400".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                    ulimits: [("nofile".to_string(), 100000)].into_iter().collect(),
                }),
                nice: Some(-10),
                io_class: Some("realtime".to_string()),
                cpu_affinity: None,
                huge_pages: None,
            }),
            _ => None,
        }
    }

    /// Detect mod support based on game type
    fn detect_mod_support(&self, game_type: &str) -> Option<ModSupport> {
        match game_type {
            "minecraft" => Some(ModSupport {
                loader: ModLoader::Bukkit,
                mods_dir: "/plugins".to_string(),
                config_dir: Some("/plugins".to_string()),
                marketplaces: vec!["spigot".to_string(), "modrinth".to_string()],
                auto_update: false,
            }),
            "rust" => Some(ModSupport {
                loader: ModLoader::Oxide,
                mods_dir: "/oxide/plugins".to_string(),
                config_dir: Some("/oxide/config".to_string()),
                marketplaces: vec!["umod".to_string(), "codefling".to_string()],
                auto_update: false,
            }),
            "valheim" => Some(ModSupport {
                loader: ModLoader::BepInEx,
                mods_dir: "/BepInEx/plugins".to_string(),
                config_dir: Some("/BepInEx/config".to_string()),
                marketplaces: vec!["thunderstore".to_string()],
                auto_update: false,
            }),
            _ => None,
        }
    }

    /// Generate update configuration from egg
    fn generate_update_config(&self, egg: &PterodactylEgg) -> Option<Updates> {
        // Check if egg uses SteamCMD
        if egg.scripts.installation.script.contains("steamcmd") {
            // Try to extract app ID from installation script
            let app_id_regex = Regex::new(r"app_update\s+(\d+)").ok()?;
            if let Some(caps) = app_id_regex.captures(&egg.scripts.installation.script) {
                if let Ok(app_id) = caps[1].parse::<u32>() {
                    return Some(Updates {
                        check: UpdateCheck::SteamCmd { app_id },
                        apply: UpdateApply::SteamCmd { app_id, beta: None },
                        auto_update: false,
                        schedule: None,
                        pre_update_command: None,
                        post_update_command: None,
                    });
                }
            }
        }
        None
    }

    /// Scan an egg's startup command and install script for risky patterns,
    /// returning one finding per match. Findings are advisory — an egg is
    /// third-party code, and the operator decides whether to deploy it.
    fn scan_egg_security(&self, egg: &PterodactylEgg) -> Result<Vec<String>> {
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

        let mut findings = Vec::new();
        for (pattern, message) in dangerous_patterns {
            let re = Regex::new(pattern)?;
            if re.is_match(&egg.startup) {
                findings.push(format!("Startup command — {}", message));
            }
            if re.is_match(&egg.scripts.installation.script) {
                findings.push(format!("Installation script — {}", message));
            }
        }

        Ok(findings)
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
            tags: if egg.features.is_empty() {
                None
            } else {
                Some(egg.features.clone())
            },
            min_panel_version: None,
            docs_url: None,
            support_url: None,
        })
    }

    fn detect_game_type(&self, egg: &PterodactylEgg) -> String {
        // An egg's startup binary is the most reliable signal of what it
        // actually runs — far more so than a free-text name. Stock eggs are
        // often named just "Rust" or "ARK", which the keyword rules below miss,
        // and a wrong game type silently costs the blueprint its mod support
        // and game-specific backup paths. Only unambiguous binary names belong
        // here.
        const STARTUP_SIGNATURES: &[(&str, &str)] = &[
            ("rustdedicated", "rust"),
            ("valheim_server", "valheim"),
            ("palworldserver", "palworld"),
            ("projectzomboid", "projectzomboid"),
            ("terrariaserver", "terraria"),
            ("7daystodieserver", "7daystodie"),
            ("shootergameserver", "ark"),
            ("conansandbox", "conanexiles"),
            ("srcds_run", "source"),
        ];

        let startup = egg.startup.to_lowercase();
        for (signature, game_type) in STARTUP_SIGNATURES {
            if startup.contains(signature) {
                return game_type.to_string();
            }
        }

        // Otherwise fall back to name, description, and docker image
        let searchable = format!(
            "{} {} {}",
            egg.name.to_lowercase(),
            egg.description.as_deref().unwrap_or("").to_lowercase(),
            egg.default_image().unwrap_or_default().to_lowercase()
        );

        // Game detection rules - ordered by specificity
        let game_patterns: &[(&[&str], &str)] = &[
            // Minecraft variants (most specific first)
            (
                &[
                    "paper",
                    "spigot",
                    "bukkit",
                    "purpur",
                    "fabric",
                    "forge",
                    "bungeecord",
                    "waterfall",
                    "velocity",
                ],
                "minecraft",
            ),
            (&["minecraft"], "minecraft"),
            // Survival games
            (&["rust dedicated", "rust server", "rustdedicated"], "rust"),
            (&["ark:", "ark survival", "arksurvival"], "ark"),
            (&["valheim"], "valheim"),
            (&["terraria"], "terraria"),
            (&["7 days to die", "7daystodie", "7dtd"], "7daystodie"),
            (&["project zomboid", "projectzomboid"], "projectzomboid"),
            (&["dayz"], "dayz"),
            (&["unturned"], "unturned"),
            (&["conan exiles", "conanexiles"], "conanexiles"),
            (
                &["the forest", "theforest", "sons of the forest"],
                "theforest",
            ),
            (&["raft"], "raft"),
            (&["palworld"], "palworld"),
            (&["enshrouded"], "enshrouded"),
            (&["v rising", "vrising"], "vrising"),
            // FPS/Competitive
            (
                &["counter-strike 2", "cs2", "csgo", "counter-strike"],
                "csgo",
            ),
            (&["team fortress", "tf2"], "tf2"),
            (&["garry's mod", "gmod", "garrysmod"], "gmod"),
            (&["left 4 dead", "l4d"], "l4d2"),
            (&["insurgency"], "insurgency"),
            (&["squad"], "squad"),
            (&["arma"], "arma"),
            // Sandbox/Creative
            (&["factorio"], "factorio"),
            (&["satisfactory"], "satisfactory"),
            (&["stationeers"], "stationeers"),
            (&["space engineers"], "spaceengineers"),
            (&["starbound"], "starbound"),
            // Racing/Sports
            (&["assetto corsa"], "assettocorsa"),
            (&["beammp", "beamng"], "beamng"),
            // MMO/RPG
            (&["fivem", "cfx"], "fivem"),
            (&["alt:v", "altv"], "altv"),
            (&["rage:mp", "ragemp"], "ragemp"),
            (&["samp", "sa-mp"], "samp"),
            (&["mta", "multi theft auto"], "mta"),
            // Other popular games
            (&["don't starve", "dontstarve"], "dontstarve"),
            (&["eco"], "eco"),
            (&["vintage story"], "vintagestory"),
            (&["barotrauma"], "barotrauma"),
            (&["corekeeper", "core keeper"], "corekeeper"),
            (&["among us"], "amongus"),
            // Bots and applications
            (&["discord", "bot"], "bot"),
            (&["teamspeak", "ts3"], "voiceserver"),
            (&["mumble"], "voiceserver"),
            // Generic source engine
            (&["source", "srcds"], "source"),
        ];

        for (patterns, game_type) in game_patterns {
            for pattern in *patterns {
                if searchable.contains(pattern) {
                    return game_type.to_string();
                }
            }
        }

        "generic".to_string()
    }

    fn convert_container(&self, egg: &PterodactylEgg) -> Result<Container> {
        // Get image from egg or provide a sensible default based on game type
        let image = egg.default_image().unwrap_or_else(|| {
            let game = self.detect_game_type(egg);
            match game.as_str() {
                "minecraft" => "ghcr.io/parkervcp/yolks:java_21".to_string(),
                "rust" => "ghcr.io/parkervcp/steamcmd:debian".to_string(),
                "ark" => "ghcr.io/parkervcp/steamcmd:debian".to_string(),
                "valheim" => "ghcr.io/parkervcp/steamcmd:debian".to_string(),
                _ => "ghcr.io/parkervcp/yolks:debian".to_string(),
            }
        });

        // Ensure image has a tag
        let image = if image.contains(':') {
            image
        } else {
            format!("{}:latest", image)
        };

        Ok(Container {
            image,
            entrypoint: None,
            environment: HashMap::new(),
            pull_policy: "if_not_present".to_string(),
            image_pull_secret: None,
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
                iops_limit: None,
                bandwidth_limit: None,
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

        // An egg's stop command is either a console command (`stop`,
        // `quit`) or `^C`, meaning "send SIGINT". Both become the shutdown
        // sequence the node runs before it resorts to signals.
        let stop = egg.config.stop.trim();
        let mut stop_signal = None;
        let mut lifecycle = lifecycle;
        if stop == "^C" || stop.eq_ignore_ascii_case("sigint") {
            stop_signal = Some("SIGINT".to_string());
        } else if stop.eq_ignore_ascii_case("sigterm") {
            stop_signal = Some("SIGTERM".to_string());
        } else if !stop.is_empty() {
            let hooks = lifecycle.get_or_insert_with(|| Lifecycle {
                pre_start: Vec::new(),
                post_start: Vec::new(),
                pre_stop: Vec::new(),
            });
            hooks.pre_stop.push(LifecycleAction::SendCommand {
                command: stop.to_string(),
                delay: None,
            });
        }

        Ok(Startup {
            command,
            args,
            working_dir: "/home/container".to_string(),
            lifecycle,
            startup_grace_period: Some("60s".to_string()),
            startup_timeout: Some("300s".to_string()),
            restart: RestartPolicy::default(),
            stop_signal,
            stop_timeout: None,
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
        Ok((
            "bash".to_string(),
            vec!["-c".to_string(), trimmed.to_string()],
        ))
    }

    fn convert_installation_script(&self, egg: &PterodactylEgg) -> Result<Lifecycle> {
        // An egg's installation script becomes the blueprint's `install`
        // block (see `convert_install`), not a pre-start hook: it is a one-off
        // that populates the server directory, and it runs in its own
        // container with its own image.
        Ok(Lifecycle {
            pre_start: vec![],
            post_start: vec![],
            pre_stop: vec![self.convert_stop_command(&egg.config.stop)],
        })
    }

    /// Carry an egg's installation script across as the blueprint's install.
    ///
    /// Everything the egg says about installing is preserved: the script, the
    /// image it runs in (installer images carry build tools the slim runtime
    /// image leaves out), and its interpreter. Pterodactyl mounts the server
    /// directory at `/mnt/server` during installation and scripts are written
    /// against that path, so the blueprint records it rather than rewriting
    /// paths inside somebody else's shell script.
    ///
    /// Returns `None` only when an egg genuinely has no installation script —
    /// previously anything that did not mention `steamcmd` was dropped on the
    /// floor, which imported a game that could never install itself.
    fn convert_install(&self, egg: &PterodactylEgg) -> Option<Install> {
        let script = egg.scripts.installation.script.trim();
        if script.is_empty() {
            return None;
        }

        let image = match egg.scripts.installation.container.trim() {
            "" => None,
            // Anything with a path is already a full reference.
            other if other.contains('/') => Some(other.to_string()),
            // A bare name (`debian:bullseye-slim`) means Docker Hub, which
            // containerd will not infer for us the way the Docker CLI does.
            other => Some(format!("docker.io/library/{}", other)),
        };

        let entrypoint = match egg.scripts.installation.entrypoint.trim() {
            "" => None,
            other => Some(other.to_string()),
        };

        Some(Install {
            image,
            entrypoint,
            script: script.to_string(),
            server_dir: Some(PTERODACTYL_INSTALL_DIR.to_string()),
            // Egg scripts routinely download whole games; Pterodactyl itself
            // imposes no limit, so this is a generous backstop rather than a
            // policy.
            timeout: Some("3600s".to_string()),
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
        variables.iter().map(|var| self.convert_variable(var)).collect()
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
            category: None,
            placeholder: None,
        })
    }

    fn parse_validation_rules(&self, rules: &str) -> Result<Vec<ValidationRule>> {
        let mut parsed_rules = Vec::new();
        let mut min_val: Option<i64> = None;
        let mut max_val: Option<i64> = None;

        // Parse Pterodactyl validation rules format
        // Common formats: "required|string|min:8", "required|numeric|between:1024,65535"
        for rule in rules.split('|') {
            let rule = rule.trim();

            if rule.starts_with("between:") {
                // Port/numeric range: "between:1024,65535"
                if let Some(range_str) = rule.strip_prefix("between:") {
                    let parts: Vec<&str> = range_str.split(',').collect();
                    if parts.len() == 2 {
                        if let (Ok(min), Ok(max)) =
                            (parts[0].parse::<u16>(), parts[1].parse::<u16>())
                        {
                            parsed_rules.push(ValidationRule::Port { range: (min, max) });
                        }
                    }
                }
            } else if rule.starts_with("regex:") {
                if let Some(pattern) = rule.strip_prefix("regex:") {
                    parsed_rules.push(ValidationRule::Regex {
                        pattern: pattern.to_string(),
                    });
                }
            } else if rule.starts_with("in:") {
                if let Some(values_str) = rule.strip_prefix("in:") {
                    let values: Vec<String> =
                        values_str.split(',').map(|s| s.trim().to_string()).collect();
                    parsed_rules.push(ValidationRule::Enum { values });
                }
            } else if rule.starts_with("min:") {
                if let Some(val_str) = rule.strip_prefix("min:") {
                    min_val = val_str.parse().ok();
                }
            } else if rule.starts_with("max:") {
                if let Some(val_str) = rule.strip_prefix("max:") {
                    max_val = val_str.parse().ok();
                }
            } else if rule == "email" {
                // Email validation - use standard email regex
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$".to_string(),
                });
            } else if rule == "url" || rule == "active_url" {
                // URL validation
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^https?://[^\s/$.?#].[^\s]*$".to_string(),
                });
            } else if rule == "ip" || rule == "ipv4" {
                // IPv4 validation
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$".to_string(),
                });
            } else if rule == "ipv6" {
                // IPv6 validation (simplified)
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}$".to_string(),
                });
            } else if rule == "alpha" {
                // Letters only
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^[a-zA-Z]+$".to_string(),
                });
            } else if rule == "alpha_num" || rule == "alphanumeric" {
                // Letters and numbers
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^[a-zA-Z0-9]+$".to_string(),
                });
            } else if rule == "alpha_dash" {
                // Letters, numbers, dashes, underscores
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^[a-zA-Z0-9_-]+$".to_string(),
                });
            } else if rule == "numeric" || rule == "integer" {
                // Numeric values
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^-?\d+$".to_string(),
                });
            } else if rule == "boolean" {
                // Boolean values
                parsed_rules.push(ValidationRule::Enum {
                    values: vec![
                        "true".to_string(),
                        "false".to_string(),
                        "1".to_string(),
                        "0".to_string(),
                    ],
                });
            } else if rule == "uuid" {
                // UUID validation
                parsed_rules.push(ValidationRule::Regex {
                    pattern: r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$".to_string(),
                });
            }
        }

        // Handle min/max if both are present (create a range)
        if let (Some(min), Some(max)) = (min_val, max_val) {
            if min >= 0 && max <= 65535 {
                parsed_rules.push(ValidationRule::Port {
                    range: (min as u16, max as u16),
                });
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
                    description: Some(var.description.clone()),
                });
            }
        }

        Ok(Networking {
            ports,
            dns: vec!["1.1.1.1".to_string(), "1.0.0.1".to_string()],
            ipv6: false,
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
            pids_limit: 1024,
            firewall_rules,
        })
    }

    fn convert_monitoring(&self, egg: &PterodactylEgg) -> Result<Option<Monitoring>> {
        // Try to detect if server has RCON or query port for health checks
        let has_rcon = egg.variables.iter().any(|v| v.name.to_lowercase().contains("rcon"));

        if has_rcon {
            Ok(Some(Monitoring {
                health_check: HealthCheck::Rcon {
                    command: "status".to_string(),
                    interval: "30s".to_string(),
                    timeout: "5s".to_string(),
                    failure_threshold: 3,
                },
                metrics: vec![],
                ready_check: None,
            }))
        } else {
            // Try TCP health check on main port
            let main_port = egg
                .variables
                .iter()
                .find(|v| {
                    v.name.to_lowercase().contains("port")
                        && !v.name.to_lowercase().contains("rcon")
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
                    ready_check: None,
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
        let (cmd, args) = converter.parse_startup_command("./server -port 25565").unwrap();
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
            features: vec![],
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

    /// A minimal but complete egg, with the risky bits templated in.
    fn egg_json(startup: &str, install_script: &str) -> String {
        format!(
            r##"{{
            "meta": {{ "version": "PTDL_v2", "update_url": null }},
            "exported_at": "2024-01-01T00:00:00+00:00",
            "name": "Minecraft Test Server",
            "author": "test@example.com",
            "description": "A test server",
            "features": null,
            "docker_images": {{ "latest": "ghcr.io/pterodactyl/yolks:java_17" }},
            "file_denylist": [],
            "startup": "{startup}",
            "config": {{
                "files": {{}},
                "startup": {{ "done": ["Done"], "user_interaction": [] }},
                "stop": "stop",
                "logs": {{ "custom": false, "location": "logs/latest.log" }},
                "file_denylist": []
            }},
            "scripts": {{
                "installation": {{
                    "script": "{install_script}",
                    "container": "alpine:latest",
                    "entrypoint": "bash"
                }}
            }},
            "variables": []
        }}"##
        )
    }

    #[test]
    fn detect_game_type_uses_the_startup_binary() {
        // A stock Pterodactyl "Rust" egg is named just "Rust" and runs on a
        // generic steamcmd image, so only the startup binary identifies it.
        // Misdetecting it as "generic" would strip Oxide mod support from the
        // imported blueprint.
        let converter = EggConverter::new();
        let egg = PterodactylEgg::from_json(&egg_json(
            "./RustDedicated -batchmode +server.port 28015",
            "echo install",
        ))
        .unwrap();
        assert_eq!(converter.detect_game_type(&egg), "rust");

        // The name-based rules still apply when the binary is unremarkable.
        let plain =
            PterodactylEgg::from_json(&egg_json("java -jar server.jar", "echo hi")).unwrap();
        assert_eq!(converter.detect_game_type(&plain), "minecraft");
    }

    #[test]
    fn imported_rust_egg_keeps_oxide_mod_support() {
        // The point of detecting the game: mod support survives the import.
        let egg = PterodactylEgg::from_json(&egg_json(
            "./RustDedicated -batchmode +server.port 28015",
            "echo install",
        ))
        .unwrap();
        let (blueprint, _) = EggConverter::new().convert_with_report(&egg).unwrap();
        let mods = blueprint.mods.expect("rust blueprint should declare mod support");
        assert_eq!(mods.mods_dir, "/oxide/plugins");
    }

    #[test]
    fn convert_with_report_surfaces_security_findings() {
        let egg = PterodactylEgg::from_json(&egg_json(
            "java -jar server.jar",
            "curl https://example.com/i.sh | bash",
        ))
        .unwrap();

        let (blueprint, warnings) = EggConverter::new().convert_with_report(&egg).unwrap();

        // The egg still converts — findings are advisory, not fatal.
        assert_eq!(blueprint.metadata.name, "Minecraft Test Server");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("Installation script"));
        assert!(warnings[0].contains("piping curl to bash"));
    }

    #[test]
    fn convert_with_report_is_quiet_for_a_clean_egg() {
        let egg =
            PterodactylEgg::from_json(&egg_json("java -jar server.jar", "echo hello")).unwrap();
        let (_, warnings) = EggConverter::new().convert_with_report(&egg).unwrap();
        assert!(warnings.is_empty(), "unexpected findings: {:?}", warnings);
    }

    #[test]
    fn security_scan_can_be_disabled() {
        let egg = PterodactylEgg::from_json(&egg_json(
            "java -jar server.jar",
            "curl https://example.com/i.sh | bash",
        ))
        .unwrap();
        let (_, warnings) =
            EggConverter::new().security_scan(false).convert_with_report(&egg).unwrap();
        assert!(warnings.is_empty());
    }
}

#[cfg(test)]
mod install_conversion_tests {
    use super::*;
    use crate::pterodactyl::{InstallationScript, PterodactylEgg, Scripts};

    fn egg_with_install(script: &str, container: &str, entrypoint: &str) -> PterodactylEgg {
        PterodactylEgg {
            comment: None,
            meta: crate::pterodactyl::Meta {
                version: "PTDL_v2".to_string(),
                update_url: None,
            },
            exported_at: "2024-01-01".to_string(),
            name: "Test Server".to_string(),
            author: "test".to_string(),
            description: None,
            features: vec![],
            docker_images: HashMap::new(),
            file_denylist: vec![],
            startup: "./start.sh".to_string(),
            config: crate::pterodactyl::Config {
                files: HashMap::new(),
                startup: crate::pterodactyl::StartupConfig {
                    done: vec![],
                    user_interaction: vec![],
                },
                stop: "stop".to_string(),
                logs: crate::pterodactyl::LogsConfig {
                    custom: false,
                    location: String::new(),
                },
                file_denylist: vec![],
            },
            scripts: Scripts {
                installation: InstallationScript {
                    script: script.to_string(),
                    container: container.to_string(),
                    entrypoint: entrypoint.to_string(),
                },
            },
            variables: vec![],
        }
    }

    /// The whole point of importing an egg is that the game it describes can
    /// then be installed. Anything that does not mention SteamCMD used to be
    /// dropped, which imported a server that could never be populated.
    #[test]
    fn a_non_steamcmd_install_script_is_kept() {
        let converter = EggConverter::new();
        let egg = egg_with_install(
            "#!/bin/bash\ncurl -sSL -o server.jar https://example.invalid/server.jar",
            "ghcr.io/pterodactyl/installers:debian",
            "bash",
        );

        let install = converter.convert_install(&egg).expect("script should be carried across");
        assert!(install.script.contains("server.jar"));
        assert_eq!(
            install.image.as_deref(),
            Some("ghcr.io/pterodactyl/installers:debian")
        );
        assert_eq!(install.entrypoint.as_deref(), Some("bash"));
        // Egg scripts are written against Pterodactyl's install mount point.
        assert_eq!(install.server_dir.as_deref(), Some("/mnt/server"));
    }

    /// A bare image name means Docker Hub. containerd does not infer that the
    /// way the Docker CLI does, so an unqualified reference would fail to pull.
    #[test]
    fn a_bare_install_image_is_qualified() {
        let converter = EggConverter::new();
        let egg = egg_with_install("echo hi", "debian:bullseye-slim", "");
        let install = converter.convert_install(&egg).unwrap();
        assert_eq!(
            install.image.as_deref(),
            Some("docker.io/library/debian:bullseye-slim")
        );
        assert_eq!(install.entrypoint, None);
    }

    #[test]
    fn an_egg_with_no_install_script_declares_no_install() {
        let converter = EggConverter::new();
        assert!(converter.convert_install(&egg_with_install("   ", "", "")).is_none());
    }
}

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pre-compiled regex for memory format validation (e.g., 512Mi, 4Gi, 1Ti)
static MEMORY_FORMAT_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\d+(\.\d+)?(Mi|Gi|Ti)$").expect("Invalid memory format regex"));

pub mod diagnostics;
pub mod errors;
pub use diagnostics::Diagnostics;
pub use errors::NexusPanelError;

/// Blueprint - Native game server configuration format
/// Superior alternative to Pterodactyl eggs with performance optimizations,
/// auto-scaling, mod support, and game-specific enhancements
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blueprint {
    /// Blueprint format version
    #[serde(default = "default_blueprint_version")]
    pub blueprint_version: String,
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
    /// Performance tuning configuration (Nexus-exclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<Performance>,
    /// Auto-scaling configuration (Nexus-exclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaling: Option<Scaling>,
    /// Mod/plugin support configuration (Nexus-exclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mods: Option<ModSupport>,
    /// Update configuration (Nexus-exclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates: Option<Updates>,
    /// Service dependencies (Nexus-exclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<Dependency>>,
    /// Clustering configuration (Nexus-exclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clustering: Option<Clustering>,
}

fn default_blueprint_version() -> String {
    "1.0".to_string()
}

/// Backward compatibility alias
pub type GameConfig = Blueprint;

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
    /// Minimum Nexus Panel version required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_panel_version: Option<String>,
    /// URL to blueprint documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    /// Support URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    /// Image pull policy: always, if_not_present, never
    #[serde(default = "default_pull_policy")]
    pub pull_policy: String,
    /// Registry authentication secret name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_pull_secret: Option<String>,
}

fn default_pull_policy() -> String {
    "if_not_present".to_string()
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
    /// IOPS limit (reads + writes per second)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iops_limit: Option<u32>,
    /// Bandwidth limit (bytes per second)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_limit: Option<String>,
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
    /// Grace period for startup (how long to wait before health checks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_grace_period: Option<String>,
    /// Timeout for startup (kill if not ready within this time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_timeout: Option<String>,
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
        destination: Option<String>,
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
    SendCommand {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        delay: Option<String>,
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
    /// Variable category for UI grouping
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Placeholder text for UI
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValidationRule {
    Port { range: (u16, u16) },
    Regex { pattern: String },
    Enum { values: Vec<String> },
    Numeric { min: Option<i64>, max: Option<i64> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Networking {
    pub ports: Vec<Port>,
    #[serde(default)]
    pub dns: Vec<String>,
    /// Enable IPv6
    #[serde(default)]
    pub ipv6: bool,
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
    /// Description shown in UI
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// Ready check - different from health check, determines when server is ready
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_check: Option<ReadyCheck>,
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
    GameQuery {
        protocol: String, // "source", "minecraft", "gamespy", etc.
        port: String,
        interval: String,
        timeout: String,
        failure_threshold: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadyCheck {
    LogPattern { pattern: String, timeout: String },
    Port { port: String, timeout: String },
    File { path: String, timeout: String },
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
    GameQuery {
        name: String,
        protocol: String,
        field: String, // "players", "max_players", "map", etc.
        interval: String,
    },
    LogParse {
        name: String,
        pattern: String,
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
    /// Retention count - number of backups to keep
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<u32>,
    /// Pre-backup command (e.g., save-all for Minecraft)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_backup_command: Option<String>,
}

// ============================================================================
// NEXUS-EXCLUSIVE FEATURES (Not available in Pterodactyl eggs)
// ============================================================================

/// Performance tuning configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Performance {
    /// JVM flags for Java-based servers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jvm: Option<JvmTuning>,
    /// Kernel parameter recommendations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<KernelTuning>,
    /// Process priority (nice value: -20 to 19)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nice: Option<i8>,
    /// IO scheduling class: realtime, best-effort, idle
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_class: Option<String>,
    /// CPU affinity - pin to specific cores
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_affinity: Option<Vec<u32>>,
    /// Huge pages configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub huge_pages: Option<HugePages>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JvmTuning {
    /// Garbage collector: g1gc, zgc, shenandoah
    #[serde(default = "default_gc")]
    pub gc: String,
    /// Initial heap size (e.g., "2G")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_heap: Option<String>,
    /// Max heap size (e.g., "4G")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_heap: Option<String>,
    /// Additional JVM flags
    #[serde(default)]
    pub flags: Vec<String>,
    /// Use Aikar's optimized flags (for Minecraft)
    #[serde(default)]
    pub aikar_flags: bool,
}

fn default_gc() -> String {
    "g1gc".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelTuning {
    /// Recommended sysctl parameters
    #[serde(default)]
    pub sysctl: HashMap<String, String>,
    /// Recommended ulimits
    #[serde(default)]
    pub ulimits: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HugePages {
    pub enabled: bool,
    /// Size: 2Mi or 1Gi
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

/// Auto-scaling configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scaling {
    /// Enable auto-scaling
    #[serde(default)]
    pub enabled: bool,
    /// Metric to scale on
    pub metric: ScalingMetric,
    /// Minimum resources when idle
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_resources: Option<ScalingResources>,
    /// Maximum resources at peak
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_resources: Option<ScalingResources>,
    /// Scale up threshold (e.g., 10 players)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_up_threshold: Option<u32>,
    /// Scale down threshold
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_down_threshold: Option<u32>,
    /// Cooldown period between scaling events
    #[serde(default = "default_cooldown")]
    pub cooldown: String,
}

fn default_cooldown() -> String {
    "5m".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalingMetric {
    PlayerCount,
    CpuUsage,
    MemoryUsage,
    Custom { command: String, parse: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingResources {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

/// Mod/plugin support configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModSupport {
    /// Mod loader type
    pub loader: ModLoader,
    /// Mods directory path
    pub mods_dir: String,
    /// Config directory path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// Supported marketplace providers
    #[serde(default)]
    pub marketplaces: Vec<String>,
    /// Auto-update mods
    #[serde(default)]
    pub auto_update: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModLoader {
    /// Umod/Oxide for Rust, 7DTD, etc.
    Oxide,
    /// Forge for Minecraft
    Forge,
    /// Fabric for Minecraft
    Fabric,
    /// Paper/Spigot plugins
    Bukkit,
    /// BepInEx for Unity games
    BepInEx,
    /// Generic mod folder
    Generic,
    /// Custom mod loader
    Custom { install_command: String },
}

/// Update configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Updates {
    /// How to check for updates
    pub check: UpdateCheck,
    /// How to apply updates
    pub apply: UpdateApply,
    /// Auto-update enabled
    #[serde(default)]
    pub auto_update: bool,
    /// Update schedule (cron expression)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Pre-update command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_update_command: Option<String>,
    /// Post-update command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_update_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UpdateCheck {
    SteamCmd { app_id: u32 },
    Http { url: String, version_path: String },
    Docker { track_tag: String },
    Command { command: String, parse: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UpdateApply {
    SteamCmd { app_id: u32, beta: Option<String> },
    Download { url: String },
    Docker { pull: bool },
    Command { command: String },
}

/// Service dependency
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    /// Dependency name
    pub name: String,
    /// Dependency type
    #[serde(rename = "type")]
    pub dep_type: DependencyType,
    /// Required (fail if not available) vs optional
    #[serde(default)]
    pub required: bool,
    /// Environment variable to inject connection string
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyType {
    Mysql,
    Mariadb,
    Postgresql,
    Redis,
    Mongodb,
    Custom { image: String, port: u16 },
}

/// Clustering configuration for multi-instance setups
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clustering {
    /// Enable clustering
    pub enabled: bool,
    /// Cluster role: primary, replica, proxy
    pub role: ClusterRole,
    /// Minimum instances
    #[serde(default = "default_min_instances")]
    pub min_instances: u32,
    /// Maximum instances
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_instances: Option<u32>,
    /// Load balancing strategy
    #[serde(default)]
    pub load_balancing: LoadBalancing,
}

fn default_min_instances() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterRole {
    Primary,
    Replica,
    Proxy,
    Standalone,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalancing {
    #[default]
    RoundRobin,
    LeastConnections,
    Random,
    IpHash,
}

impl Blueprint {
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
                        ValidationRule::Regex { pattern }
                            if regex::Regex::new(pattern).is_err() =>
                        {
                            errors.push(format!(
                                "  • Variable '{}': invalid regex pattern '{}'",
                                var.name, pattern
                            ));
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

        // Validate scaling configuration
        if let Some(scaling) = &self.scaling {
            if scaling.enabled {
                if let (Some(min), Some(max)) = (&scaling.min_resources, &scaling.max_resources) {
                    if let (Some(min_cpu), Some(max_cpu)) = (min.cpu, max.cpu) {
                        if min_cpu > max_cpu {
                            errors.push("  • Scaling: min CPU cannot exceed max CPU".to_string());
                        }
                    }
                }
            }
        }

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
        MEMORY_FORMAT_REGEX.is_match(mem)
    }

    /// Export to YAML
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// Load from YAML
    pub fn from_yaml(yaml: &str) -> anyhow::Result<Self> {
        let config: Blueprint = serde_yaml::from_str(yaml)?;
        config.validate()?;
        Ok(config)
    }

    /// Generate optimized JVM flags based on memory allocation
    pub fn generate_jvm_flags(&self) -> Option<Vec<String>> {
        let perf = self.performance.as_ref()?;
        let jvm = perf.jvm.as_ref()?;

        let mut flags = Vec::new();

        // Heap settings
        if let Some(ref initial) = jvm.initial_heap {
            flags.push(format!("-Xms{}", initial));
        }
        if let Some(ref max) = jvm.max_heap {
            flags.push(format!("-Xmx{}", max));
        }

        // Garbage collector
        match jvm.gc.as_str() {
            "g1gc" => {
                flags.push("-XX:+UseG1GC".to_string());
                if jvm.aikar_flags {
                    // Aikar's optimized G1GC flags for Minecraft
                    flags.extend(vec![
                        "-XX:+ParallelRefProcEnabled".to_string(),
                        "-XX:MaxGCPauseMillis=200".to_string(),
                        "-XX:+UnlockExperimentalVMOptions".to_string(),
                        "-XX:+DisableExplicitGC".to_string(),
                        "-XX:+AlwaysPreTouch".to_string(),
                        "-XX:G1NewSizePercent=30".to_string(),
                        "-XX:G1MaxNewSizePercent=40".to_string(),
                        "-XX:G1HeapRegionSize=8M".to_string(),
                        "-XX:G1ReservePercent=20".to_string(),
                        "-XX:G1HeapWastePercent=5".to_string(),
                        "-XX:G1MixedGCCountTarget=4".to_string(),
                        "-XX:InitiatingHeapOccupancyPercent=15".to_string(),
                        "-XX:G1MixedGCLiveThresholdPercent=90".to_string(),
                        "-XX:G1RSetUpdatingPauseTimePercent=5".to_string(),
                        "-XX:SurvivorRatio=32".to_string(),
                        "-XX:+PerfDisableSharedMem".to_string(),
                        "-XX:MaxTenuringThreshold=1".to_string(),
                    ]);
                }
            }
            "zgc" => {
                flags.push("-XX:+UseZGC".to_string());
                flags.push("-XX:+UnlockExperimentalVMOptions".to_string());
            }
            "shenandoah" => {
                flags.push("-XX:+UseShenandoahGC".to_string());
            }
            _ => {}
        }

        // Additional custom flags
        flags.extend(jvm.flags.clone());

        Some(flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_deserialize() {
        let config = Blueprint {
            blueprint_version: "1.0".to_string(),
            metadata: Metadata {
                id: "rust-test".to_string(),
                name: "Rust Test Server".to_string(),
                version: "1.0.0".to_string(),
                game: "rust".to_string(),
                author: "test".to_string(),
                description: None,
                tags: None,
                min_panel_version: None,
                docs_url: None,
                support_url: None,
            },
            container: Container {
                image: "ghcr.io/test/rust:latest".to_string(),
                entrypoint: None,
                environment: HashMap::new(),
                pull_policy: "if_not_present".to_string(),
                image_pull_secret: None,
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
                    iops_limit: None,
                    bandwidth_limit: None,
                },
            },
            startup: Startup {
                command: "./RustDedicated".to_string(),
                args: vec!["-batchmode".to_string()],
                working_dir: "/home/container".to_string(),
                lifecycle: None,
                startup_grace_period: None,
                startup_timeout: None,
            },
            variables: vec![],
            networking: Networking {
                ports: vec![],
                dns: vec![],
                ipv6: false,
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
            performance: None,
            scaling: None,
            mods: None,
            updates: None,
            dependencies: None,
            clustering: None,
        };

        let yaml = config.to_yaml().unwrap();
        let parsed = Blueprint::from_yaml(&yaml).unwrap();
        assert_eq!(config.metadata.id, parsed.metadata.id);
    }

    #[test]
    fn test_jvm_flag_generation() {
        let config = Blueprint {
            blueprint_version: "1.0".to_string(),
            metadata: Metadata {
                id: "mc-test".to_string(),
                name: "Minecraft Test".to_string(),
                version: "1.0.0".to_string(),
                game: "minecraft".to_string(),
                author: "test".to_string(),
                description: None,
                tags: None,
                min_panel_version: None,
                docs_url: None,
                support_url: None,
            },
            container: Container {
                image: "ghcr.io/test/java:17".to_string(),
                entrypoint: None,
                environment: HashMap::new(),
                pull_policy: "if_not_present".to_string(),
                image_pull_secret: None,
            },
            resources: Resources {
                cpu: CpuResources {
                    min: 1000,
                    max: 2000,
                    shares: 1024,
                },
                memory: MemoryResources {
                    min: "2Gi".to_string(),
                    max: "4Gi".to_string(),
                    swap: None,
                },
                disk: DiskResources {
                    min: "10Gi".to_string(),
                    io_priority: "normal".to_string(),
                    iops_limit: None,
                    bandwidth_limit: None,
                },
            },
            startup: Startup {
                command: "java".to_string(),
                args: vec![],
                working_dir: "/home/container".to_string(),
                lifecycle: None,
                startup_grace_period: None,
                startup_timeout: None,
            },
            variables: vec![],
            networking: Networking {
                ports: vec![],
                dns: vec![],
                ipv6: false,
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
            performance: Some(Performance {
                jvm: Some(JvmTuning {
                    gc: "g1gc".to_string(),
                    initial_heap: Some("2G".to_string()),
                    max_heap: Some("4G".to_string()),
                    flags: vec![],
                    aikar_flags: true,
                }),
                kernel: None,
                nice: None,
                io_class: None,
                cpu_affinity: None,
                huge_pages: None,
            }),
            scaling: None,
            mods: None,
            updates: None,
            dependencies: None,
            clustering: None,
        };

        let flags = config.generate_jvm_flags().unwrap();
        assert!(flags.contains(&"-Xms2G".to_string()));
        assert!(flags.contains(&"-Xmx4G".to_string()));
        assert!(flags.contains(&"-XX:+UseG1GC".to_string()));
        assert!(flags.contains(&"-XX:MaxGCPauseMillis=200".to_string())); // Aikar flag
    }
}

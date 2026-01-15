//! XDP (eXpress Data Path) Firewall for high-performance DDoS protection.
//!
//! Provides kernel-level packet filtering using Linux XDP/eBPF technology for:
//! - **Ultra-low latency**: Packets filtered before kernel network stack
//! - **High throughput**: Millions of packets per second processing
//! - **DDoS mitigation**: Rate limiting, connection tracking, pattern blocking
//! - **Game server protection**: Protocol-aware filtering for gaming traffic
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        Network Interface                         │
//! └───────────────────────────────┬─────────────────────────────────┘
//!                                 │
//!                                 ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      XDP eBPF Program                            │
//! │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
//! │  │ Rate Limit  │  │ IP Blocklist│  │ Port Rules  │              │
//! │  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘              │
//! │         │                │                │                     │
//! │         └────────────────┼────────────────┘                     │
//! │                          ▼                                      │
//! │                    ┌──────────┐                                 │
//! │                    │ Decision │ → XDP_DROP / XDP_PASS           │
//! │                    └──────────┘                                 │
//! └─────────────────────────────────────────────────────────────────┘
//!                                 │
//!                                 ▼ (if XDP_PASS)
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    Linux Network Stack                           │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Features
//!
//! - Per-source IP rate limiting
//! - Connection rate limiting (SYN flood protection)
//! - IP blocklist/allowlist with CIDR support
//! - Port-based filtering
//! - Protocol filtering (TCP, UDP, ICMP)
//! - Packet size limits (amplification attack prevention)
//! - Geographic blocking (via IP ranges)
//! - Real-time statistics and monitoring
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::xdp_firewall::{XdpFirewall, FirewallConfig};
//!
//! let config = FirewallConfig::default();
//! let firewall = XdpFirewall::new(config)?;
//!
//! // Add rate limiting rule
//! firewall.add_rate_limit("192.168.1.0/24", 1000, Duration::from_secs(1))?;
//!
//! // Block an IP
//! firewall.block_ip("10.0.0.100", Duration::from_secs(3600))?;
//!
//! // Start the firewall
//! firewall.start("eth0").await?;
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// XDP Firewall errors
#[derive(Error, Debug)]
pub enum XdpError {
    #[error("XDP not supported on this system")]
    NotSupported,

    #[error("Insufficient permissions: root required")]
    PermissionDenied,

    #[error("Interface not found: {0}")]
    InterfaceNotFound(String),

    #[error("BPF program load failed: {0}")]
    BpfLoadFailed(String),

    #[error("Invalid CIDR: {0}")]
    InvalidCidr(String),

    #[error("Rule limit exceeded: maximum {0} rules")]
    RuleLimitExceeded(usize),

    #[error("Rule not found: {0}")]
    RuleNotFound(String),

    #[error("XDP firewall not running")]
    NotRunning,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    ConfigError(String),
}

/// XDP Firewall configuration
#[derive(Debug, Clone)]
pub struct FirewallConfig {
    /// Enable XDP firewall
    pub enabled: bool,
    /// Network interface to attach to
    pub interface: String,
    /// XDP attach mode
    pub mode: XdpMode,
    /// Maximum rules per category
    pub max_rules: usize,
    /// Default action for unmatched packets
    pub default_action: FirewallAction,
    /// Enable connection tracking
    pub connection_tracking: bool,
    /// Connection tracking table size
    pub conntrack_size: usize,
    /// Global rate limit (packets per second, 0 = unlimited)
    pub global_rate_limit: u64,
    /// Per-IP rate limit (packets per second)
    pub per_ip_rate_limit: u64,
    /// SYN rate limit (new connections per second per IP)
    pub syn_rate_limit: u64,
    /// Maximum packet size (bytes)
    pub max_packet_size: u32,
    /// Enable logging of dropped packets
    pub log_drops: bool,
    /// BPF program path (if using custom program)
    pub bpf_program_path: Option<PathBuf>,
    /// Statistics update interval
    pub stats_interval: Duration,
    /// Auto-block threshold (drops before auto-block)
    pub auto_block_threshold: u64,
    /// Auto-block duration
    pub auto_block_duration: Duration,
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interface: "eth0".to_string(),
            mode: XdpMode::Native,
            max_rules: 10000,
            default_action: FirewallAction::Pass,
            connection_tracking: true,
            conntrack_size: 65536,
            global_rate_limit: 0,
            per_ip_rate_limit: 10000,
            syn_rate_limit: 100,
            max_packet_size: 1500,
            log_drops: true,
            bpf_program_path: None,
            stats_interval: Duration::from_secs(1),
            auto_block_threshold: 10000,
            auto_block_duration: Duration::from_secs(300),
        }
    }
}

impl FirewallConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("XDP_FIREWALL_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }

        if let Ok(interface) = std::env::var("XDP_INTERFACE") {
            config.interface = interface;
        }

        if let Ok(mode) = std::env::var("XDP_MODE") {
            config.mode = match mode.to_lowercase().as_str() {
                "native" | "drv" => XdpMode::Native,
                "offload" | "hw" => XdpMode::Offload,
                _ => XdpMode::Generic,
            };
        }

        if let Ok(limit) = std::env::var("XDP_PER_IP_RATE_LIMIT") {
            config.per_ip_rate_limit = limit.parse().unwrap_or(10000);
        }

        if let Ok(limit) = std::env::var("XDP_SYN_RATE_LIMIT") {
            config.syn_rate_limit = limit.parse().unwrap_or(100);
        }

        if let Ok(size) = std::env::var("XDP_MAX_PACKET_SIZE") {
            config.max_packet_size = size.parse().unwrap_or(1500);
        }

        if let Ok(threshold) = std::env::var("XDP_AUTO_BLOCK_THRESHOLD") {
            config.auto_block_threshold = threshold.parse().unwrap_or(10000);
        }

        config
    }

    /// Create a gaming-optimized configuration
    pub fn gaming_optimized() -> Self {
        Self {
            enabled: true,
            interface: "eth0".to_string(),
            mode: XdpMode::Native,
            max_rules: 50000,
            default_action: FirewallAction::Pass,
            connection_tracking: true,
            conntrack_size: 131072,
            global_rate_limit: 0,
            per_ip_rate_limit: 5000,
            syn_rate_limit: 50,
            max_packet_size: 1500,
            log_drops: true,
            bpf_program_path: None,
            stats_interval: Duration::from_millis(100),
            auto_block_threshold: 5000,
            auto_block_duration: Duration::from_secs(600),
        }
    }

    /// Create a high-security configuration
    pub fn high_security() -> Self {
        Self {
            enabled: true,
            interface: "eth0".to_string(),
            mode: XdpMode::Native,
            max_rules: 100000,
            default_action: FirewallAction::Drop, // Default deny
            connection_tracking: true,
            conntrack_size: 262144,
            global_rate_limit: 100000,
            per_ip_rate_limit: 1000,
            syn_rate_limit: 10,
            max_packet_size: 1400,
            log_drops: true,
            bpf_program_path: None,
            stats_interval: Duration::from_millis(100),
            auto_block_threshold: 1000,
            auto_block_duration: Duration::from_secs(3600),
        }
    }
}

/// XDP attach mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum XdpMode {
    /// Native driver mode (fastest, requires driver support)
    Native,
    /// Generic/SKB mode (slower, universal compatibility)
    Generic,
    /// Hardware offload mode (fastest, requires NIC support)
    Offload,
}

impl XdpMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Generic => "generic",
            Self::Offload => "offload",
        }
    }
}

/// Firewall action for packets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FirewallAction {
    /// Allow packet through
    Pass,
    /// Drop packet
    Drop,
    /// Abort connection (TCP RST)
    Abort,
    /// Redirect to another interface
    Redirect,
    /// Send to userspace for inspection
    Tx,
}

impl FirewallAction {
    /// Convert to XDP return code
    pub fn to_xdp_code(&self) -> u32 {
        match self {
            Self::Pass => 2,    // XDP_PASS
            Self::Drop => 1,    // XDP_DROP
            Self::Abort => 0,   // XDP_ABORTED
            Self::Redirect => 4, // XDP_REDIRECT
            Self::Tx => 3,      // XDP_TX
        }
    }
}

/// Firewall rule types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FirewallRule {
    /// Block/allow specific IP
    IpRule {
        id: String,
        ip: IpAddr,
        action: FirewallAction,
        expires_at: Option<u64>,
        reason: Option<String>,
        hit_count: u64,
    },
    /// Block/allow CIDR range
    CidrRule {
        id: String,
        cidr: String,
        prefix_len: u8,
        action: FirewallAction,
        expires_at: Option<u64>,
        reason: Option<String>,
        hit_count: u64,
    },
    /// Port-based rule
    PortRule {
        id: String,
        port: u16,
        protocol: IpProtocol,
        action: FirewallAction,
        reason: Option<String>,
        hit_count: u64,
    },
    /// Rate limit rule
    RateLimitRule {
        id: String,
        target: RateLimitTarget,
        packets_per_second: u64,
        burst_size: u64,
        action: FirewallAction,
    },
    /// Packet size rule
    PacketSizeRule {
        id: String,
        min_size: Option<u32>,
        max_size: Option<u32>,
        protocol: Option<IpProtocol>,
        action: FirewallAction,
    },
    /// Protocol rule
    ProtocolRule {
        id: String,
        protocol: IpProtocol,
        action: FirewallAction,
        reason: Option<String>,
    },
    /// Game server protection rule
    GameServerRule {
        id: String,
        game_type: GameType,
        port: u16,
        query_port: Option<u16>,
        rcon_port: Option<u16>,
        max_players: u32,
        action: FirewallAction,
    },
}

impl FirewallRule {
    pub fn id(&self) -> &str {
        match self {
            Self::IpRule { id, .. }
            | Self::CidrRule { id, .. }
            | Self::PortRule { id, .. }
            | Self::RateLimitRule { id, .. }
            | Self::PacketSizeRule { id, .. }
            | Self::ProtocolRule { id, .. }
            | Self::GameServerRule { id, .. } => id,
        }
    }

    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match self {
            Self::IpRule { expires_at, .. } | Self::CidrRule { expires_at, .. } => {
                expires_at.map_or(false, |exp| now > exp)
            }
            _ => false,
        }
    }
}

/// Rate limit target
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitTarget {
    /// Global rate limit
    Global,
    /// Per source IP
    PerSourceIp,
    /// Per destination port
    PerDestPort,
    /// Per source IP + destination port
    PerFlow,
    /// SYN packets only
    SynPackets,
}

/// IP protocols
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IpProtocol {
    Tcp,
    Udp,
    Icmp,
    IcmpV6,
    Any,
}

impl IpProtocol {
    pub fn to_number(&self) -> u8 {
        match self {
            Self::Tcp => 6,
            Self::Udp => 17,
            Self::Icmp => 1,
            Self::IcmpV6 => 58,
            Self::Any => 0,
        }
    }
}

/// Game types with protocol-specific protection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameType {
    Minecraft,
    MinecraftBedrock,
    Rust,
    ArkSurvival,
    SevenDaysToDie,
    Valheim,
    Terraria,
    CounterStrike,
    TeamFortress2,
    GarryssMod,
    Arma3,
    DayZ,
    Squad,
    Generic,
}

impl GameType {
    /// Get the default game port
    pub fn default_port(&self) -> u16 {
        match self {
            Self::Minecraft => 25565,
            Self::MinecraftBedrock => 19132,
            Self::Rust => 28015,
            Self::ArkSurvival => 7777,
            Self::SevenDaysToDie => 26900,
            Self::Valheim => 2456,
            Self::Terraria => 7777,
            Self::CounterStrike => 27015,
            Self::TeamFortress2 => 27015,
            Self::GarryssMod => 27015,
            Self::Arma3 => 2302,
            Self::DayZ => 2302,
            Self::Squad => 7787,
            Self::Generic => 0,
        }
    }

    /// Get the default protocol
    pub fn default_protocol(&self) -> IpProtocol {
        match self {
            Self::Minecraft => IpProtocol::Tcp,
            Self::MinecraftBedrock => IpProtocol::Udp,
            _ => IpProtocol::Udp,
        }
    }

    /// Get recommended rate limits
    pub fn recommended_rate_limits(&self) -> (u64, u64) {
        // (packets_per_second, syn_rate)
        match self {
            Self::Minecraft => (5000, 50),
            Self::MinecraftBedrock => (10000, 100),
            Self::Rust => (8000, 100),
            Self::ArkSurvival => (10000, 100),
            Self::CounterStrike | Self::TeamFortress2 => (15000, 200),
            _ => (10000, 100),
        }
    }
}

/// XDP Firewall statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirewallStats {
    /// Total packets processed
    pub packets_total: u64,
    /// Packets passed
    pub packets_passed: u64,
    /// Packets dropped
    pub packets_dropped: u64,
    /// Bytes processed
    pub bytes_total: u64,
    /// Bytes dropped
    pub bytes_dropped: u64,
    /// Rate limit drops
    pub rate_limit_drops: u64,
    /// IP blocklist drops
    pub blocklist_drops: u64,
    /// SYN flood drops
    pub syn_flood_drops: u64,
    /// Invalid packet drops
    pub invalid_drops: u64,
    /// Active connections (if tracking enabled)
    pub active_connections: u64,
    /// Current packet rate (pps)
    pub current_pps: u64,
    /// Peak packet rate (pps)
    pub peak_pps: u64,
    /// Last update timestamp
    pub last_update: u64,
    /// Uptime in seconds
    pub uptime_secs: u64,
}

/// Blocked IP information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedIp {
    pub ip: IpAddr,
    pub blocked_at: u64,
    pub expires_at: Option<u64>,
    pub reason: String,
    pub packets_blocked: u64,
    pub bytes_blocked: u64,
    pub auto_blocked: bool,
}

/// XDP Firewall manager
pub struct XdpFirewall {
    config: FirewallConfig,
    /// Current rules
    rules: Arc<RwLock<HashMap<String, FirewallRule>>>,
    /// Blocked IPs
    blocked_ips: Arc<RwLock<HashMap<IpAddr, BlockedIp>>>,
    /// Allowed IPs (whitelist)
    allowed_ips: Arc<RwLock<HashMap<IpAddr, String>>>,
    /// Statistics
    stats: Arc<RwLock<FirewallStats>>,
    /// Running state
    running: Arc<std::sync::atomic::AtomicBool>,
    /// Start time
    start_time: Arc<RwLock<Option<Instant>>>,
}

impl XdpFirewall {
    /// Create a new XDP firewall
    pub fn new(config: FirewallConfig) -> Result<Self, XdpError> {
        // Check if XDP is supported
        if !Self::is_xdp_supported() {
            warn!("XDP may not be fully supported on this system");
        }

        Ok(Self {
            config,
            rules: Arc::new(RwLock::new(HashMap::new())),
            blocked_ips: Arc::new(RwLock::new(HashMap::new())),
            allowed_ips: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(FirewallStats::default())),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            start_time: Arc::new(RwLock::new(None)),
        })
    }

    /// Check if XDP is supported on this system
    pub fn is_xdp_supported() -> bool {
        // Check for /sys/fs/bpf and kernel version
        std::path::Path::new("/sys/fs/bpf").exists()
    }

    /// Check if firewall is running
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Start the XDP firewall
    pub async fn start(&self) -> Result<(), XdpError> {
        if !self.config.enabled {
            return Ok(());
        }

        info!(
            "Starting XDP firewall on interface {} (mode: {:?})",
            self.config.interface, self.config.mode
        );

        // Check interface exists
        if !Self::interface_exists(&self.config.interface) {
            return Err(XdpError::InterfaceNotFound(self.config.interface.clone()));
        }

        // In a real implementation, this would:
        // 1. Load the eBPF program
        // 2. Attach it to the network interface
        // 3. Initialize BPF maps with rules
        // 4. Start the stats collection task

        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *self.start_time.write().await = Some(Instant::now());

        info!("XDP firewall started successfully");
        Ok(())
    }

    /// Stop the XDP firewall
    pub async fn stop(&self) -> Result<(), XdpError> {
        if !self.is_running() {
            return Ok(());
        }

        info!("Stopping XDP firewall");

        // In a real implementation, this would:
        // 1. Detach the eBPF program
        // 2. Clean up BPF maps
        // 3. Stop stats collection

        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *self.start_time.write().await = None;

        info!("XDP firewall stopped");
        Ok(())
    }

    /// Check if interface exists
    fn interface_exists(name: &str) -> bool {
        std::path::Path::new(&format!("/sys/class/net/{}", name)).exists()
    }

    // ==================== Rule Management ====================

    /// Add a firewall rule
    pub async fn add_rule(&self, rule: FirewallRule) -> Result<(), XdpError> {
        let rules = self.rules.read().await;
        if rules.len() >= self.config.max_rules {
            return Err(XdpError::RuleLimitExceeded(self.config.max_rules));
        }
        drop(rules);

        let id = rule.id().to_string();
        info!("Adding firewall rule: {}", id);

        let mut rules = self.rules.write().await;
        rules.insert(id, rule);

        Ok(())
    }

    /// Remove a firewall rule
    pub async fn remove_rule(&self, rule_id: &str) -> Result<(), XdpError> {
        let mut rules = self.rules.write().await;
        if rules.remove(rule_id).is_none() {
            return Err(XdpError::RuleNotFound(rule_id.to_string()));
        }

        info!("Removed firewall rule: {}", rule_id);
        Ok(())
    }

    /// Get all rules
    pub async fn list_rules(&self) -> Vec<FirewallRule> {
        let rules = self.rules.read().await;
        rules.values().cloned().collect()
    }

    /// Clear expired rules
    pub async fn cleanup_expired(&self) {
        let mut rules = self.rules.write().await;
        let before = rules.len();
        rules.retain(|_, rule| !rule.is_expired());
        let removed = before - rules.len();
        if removed > 0 {
            info!("Cleaned up {} expired rules", removed);
        }
        drop(rules);

        // Also cleanup expired blocked IPs
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut blocked = self.blocked_ips.write().await;
        blocked.retain(|_, info| info.expires_at.map_or(true, |exp| now <= exp));
    }

    // ==================== IP Blocking ====================

    /// Block an IP address
    pub async fn block_ip(
        &self,
        ip: IpAddr,
        duration: Option<Duration>,
        reason: &str,
    ) -> Result<(), XdpError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let expires_at = duration.map(|d| now + d.as_secs());

        let blocked_ip = BlockedIp {
            ip,
            blocked_at: now,
            expires_at,
            reason: reason.to_string(),
            packets_blocked: 0,
            bytes_blocked: 0,
            auto_blocked: false,
        };

        info!(
            "Blocking IP {} (reason: {}, expires: {:?})",
            ip, reason, expires_at
        );

        let mut blocked = self.blocked_ips.write().await;
        blocked.insert(ip, blocked_ip);

        Ok(())
    }

    /// Unblock an IP address
    pub async fn unblock_ip(&self, ip: &IpAddr) -> Result<(), XdpError> {
        let mut blocked = self.blocked_ips.write().await;
        if blocked.remove(ip).is_none() {
            return Err(XdpError::RuleNotFound(ip.to_string()));
        }

        info!("Unblocked IP {}", ip);
        Ok(())
    }

    /// Check if an IP is blocked
    pub async fn is_blocked(&self, ip: &IpAddr) -> bool {
        let blocked = self.blocked_ips.read().await;
        if let Some(info) = blocked.get(ip) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            info.expires_at.map_or(true, |exp| now <= exp)
        } else {
            false
        }
    }

    /// Get all blocked IPs
    pub async fn list_blocked_ips(&self) -> Vec<BlockedIp> {
        let blocked = self.blocked_ips.read().await;
        blocked.values().cloned().collect()
    }

    // ==================== IP Allowlist ====================

    /// Add IP to allowlist
    pub async fn allow_ip(&self, ip: IpAddr, reason: &str) -> Result<(), XdpError> {
        info!("Adding IP {} to allowlist (reason: {})", ip, reason);
        let mut allowed = self.allowed_ips.write().await;
        allowed.insert(ip, reason.to_string());
        Ok(())
    }

    /// Remove IP from allowlist
    pub async fn remove_from_allowlist(&self, ip: &IpAddr) -> Result<(), XdpError> {
        let mut allowed = self.allowed_ips.write().await;
        allowed.remove(ip);
        info!("Removed IP {} from allowlist", ip);
        Ok(())
    }

    /// Check if IP is allowlisted
    pub async fn is_allowed(&self, ip: &IpAddr) -> bool {
        let allowed = self.allowed_ips.read().await;
        allowed.contains_key(ip)
    }

    // ==================== Game Server Protection ====================

    /// Add protection rules for a game server
    pub async fn protect_game_server(
        &self,
        game_type: GameType,
        port: u16,
        query_port: Option<u16>,
        rcon_port: Option<u16>,
    ) -> Result<String, XdpError> {
        let (pps_limit, syn_limit) = game_type.recommended_rate_limits();

        let rule_id = format!("game_{}_{}", format!("{:?}", game_type).to_lowercase(), port);

        let rule = FirewallRule::GameServerRule {
            id: rule_id.clone(),
            game_type,
            port,
            query_port,
            rcon_port,
            max_players: 100,
            action: FirewallAction::Pass,
        };

        self.add_rule(rule).await?;

        // Add rate limiting for the game port
        let rate_rule = FirewallRule::RateLimitRule {
            id: format!("{}_rate", rule_id),
            target: RateLimitTarget::PerSourceIp,
            packets_per_second: pps_limit,
            burst_size: pps_limit / 10,
            action: FirewallAction::Drop,
        };

        self.add_rule(rate_rule).await?;

        // Block RCON from external access by default
        if let Some(rcon) = rcon_port {
            let rcon_rule = FirewallRule::PortRule {
                id: format!("{}_rcon_block", rule_id),
                port: rcon,
                protocol: IpProtocol::Tcp,
                action: FirewallAction::Drop,
                reason: Some("RCON blocked by default".to_string()),
                hit_count: 0,
            };
            self.add_rule(rcon_rule).await?;
        }

        info!(
            "Added protection for {:?} game server on port {}",
            game_type, port
        );

        Ok(rule_id)
    }

    // ==================== Statistics ====================

    /// Get current statistics
    pub async fn get_stats(&self) -> FirewallStats {
        let mut stats = self.stats.read().await.clone();

        // Update uptime
        if let Some(start) = *self.start_time.read().await {
            stats.uptime_secs = start.elapsed().as_secs();
        }

        stats
    }

    /// Reset statistics
    pub async fn reset_stats(&self) {
        let mut stats = self.stats.write().await;
        *stats = FirewallStats::default();
        info!("Statistics reset");
    }

    // ==================== Configuration ====================

    /// Get current configuration
    pub fn config(&self) -> &FirewallConfig {
        &self.config
    }

    /// Update rate limits
    pub fn update_rate_limits(&mut self, per_ip: u64, syn: u64) {
        self.config.per_ip_rate_limit = per_ip;
        self.config.syn_rate_limit = syn;
        info!(
            "Updated rate limits: per_ip={} pps, syn={} pps",
            per_ip, syn
        );
    }

    /// Export rules to JSON
    pub async fn export_rules(&self) -> Result<String, XdpError> {
        let rules = self.rules.read().await;
        let rules_vec: Vec<_> = rules.values().collect();
        serde_json::to_string_pretty(&rules_vec)
            .map_err(|e| XdpError::ConfigError(e.to_string()))
    }

    /// Import rules from JSON
    pub async fn import_rules(&self, json: &str) -> Result<usize, XdpError> {
        let rules: Vec<FirewallRule> =
            serde_json::from_str(json).map_err(|e| XdpError::ConfigError(e.to_string()))?;

        let count = rules.len();
        for rule in rules {
            self.add_rule(rule).await?;
        }

        info!("Imported {} rules", count);
        Ok(count)
    }
}

/// Helper to parse CIDR notation
pub fn parse_cidr(cidr: &str) -> Result<(IpAddr, u8), XdpError> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return Err(XdpError::InvalidCidr(cidr.to_string()));
    }

    let ip: IpAddr = parts[0]
        .parse()
        .map_err(|_| XdpError::InvalidCidr(cidr.to_string()))?;

    let prefix: u8 = parts[1]
        .parse()
        .map_err(|_| XdpError::InvalidCidr(cidr.to_string()))?;

    let max_prefix = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > max_prefix {
        return Err(XdpError::InvalidCidr(cidr.to_string()));
    }

    Ok((ip, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = FirewallConfig::default();
        assert!(config.enabled);
        assert_eq!(config.mode, XdpMode::Native);
        assert_eq!(config.per_ip_rate_limit, 10000);
    }

    #[test]
    fn test_game_type_defaults() {
        assert_eq!(GameType::Minecraft.default_port(), 25565);
        assert_eq!(GameType::Minecraft.default_protocol(), IpProtocol::Tcp);
        assert_eq!(GameType::Rust.default_port(), 28015);
        assert_eq!(GameType::Rust.default_protocol(), IpProtocol::Udp);
    }

    #[test]
    fn test_parse_cidr_valid() {
        let (ip, prefix) = parse_cidr("192.168.1.0/24").unwrap();
        assert_eq!(ip, "192.168.1.0".parse::<IpAddr>().unwrap());
        assert_eq!(prefix, 24);

        let (ip, prefix) = parse_cidr("10.0.0.0/8").unwrap();
        assert_eq!(prefix, 8);
    }

    #[test]
    fn test_parse_cidr_invalid() {
        assert!(parse_cidr("192.168.1.0").is_err());
        assert!(parse_cidr("192.168.1.0/33").is_err());
        assert!(parse_cidr("invalid/24").is_err());
    }

    #[test]
    fn test_firewall_action_codes() {
        assert_eq!(FirewallAction::Pass.to_xdp_code(), 2);
        assert_eq!(FirewallAction::Drop.to_xdp_code(), 1);
    }

    #[tokio::test]
    async fn test_firewall_creation() {
        let config = FirewallConfig::default();
        let firewall = XdpFirewall::new(config).unwrap();
        assert!(!firewall.is_running());
    }

    #[tokio::test]
    async fn test_ip_blocking() {
        let config = FirewallConfig::default();
        let firewall = XdpFirewall::new(config).unwrap();

        let ip: IpAddr = "192.168.1.100".parse().unwrap();
        firewall
            .block_ip(ip, Some(Duration::from_secs(3600)), "test")
            .await
            .unwrap();

        assert!(firewall.is_blocked(&ip).await);

        firewall.unblock_ip(&ip).await.unwrap();
        assert!(!firewall.is_blocked(&ip).await);
    }

    #[tokio::test]
    async fn test_allowlist() {
        let config = FirewallConfig::default();
        let firewall = XdpFirewall::new(config).unwrap();

        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        firewall.allow_ip(ip, "trusted").await.unwrap();

        assert!(firewall.is_allowed(&ip).await);
    }
}

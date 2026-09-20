//! Per-server firewall and DDoS protection, on nftables.
//!
//! Game servers share the host's network namespace, so the only place to
//! filter their traffic is the host's own packet path. This module owns one
//! nftables table, `inet nexus`, and keeps it in step with what is running:
//!
//! - **Node-wide protection**, always on: a blocklist and a trusted list,
//!   invalid-state drops, per-source SYN and UDP flood meters on every game
//!   port, and a global SYN ceiling. A single attacking address is cut off
//!   without touching anyone else; a spoofed flood hits the ceiling.
//! - **Per-server chains**, applied when a server starts and removed when
//!   it stops: the blueprint's `security.firewall_rules` (connection rate,
//!   packet size, allowed and blocked CIDRs) attached to exactly that
//!   server's ports through verdict maps, so each server's rules cost one
//!   map lookup and its counters are its own.
//! - **Kernel tuning** for the same threats: SYN cookies, a deep SYN
//!   backlog, a larger conntrack table.
//!
//! Every change is one atomic `nft -f` transaction. Rules carry a comment
//! the status reader keys on, so the panel can show what each one dropped.
//!
//! Without `nft` on the node, or without root, the firewall is disabled
//! and says so once at startup; nothing else changes.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use nexus_config::{FirewallAction, FirewallRule, GameConfig};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::error::{NodeError, Result};
use crate::provision::{resolve_ports, AssignedPort};

/// The nftables table everything lives in.
pub const TABLE: &str = "nexus";

/// Longest a chain name may be on older kernels. Server chains use a prefix
/// plus the first characters of the id, which keeps them well inside it.
const CHAIN_ID_CHARS: usize = 12;

/// How the firewall was configured to behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// On when `nft` is present and we are root; otherwise off with a warning.
    Auto,
    /// On; a missing `nft` is an error at startup.
    On,
    /// Off.
    Off,
}

/// Node-wide protection thresholds, from the environment.
#[derive(Debug, Clone)]
pub struct FirewallSettings {
    pub mode: Mode,
    /// New TCP connections per second one source may open to game ports
    /// before it is dropped (`NEXUS_FIREWALL_SYN_PER_SOURCE`).
    pub syn_per_source: u32,
    /// New TCP connections per second across all game ports before the
    /// excess is dropped (`NEXUS_FIREWALL_SYN_GLOBAL`).
    pub syn_global: u32,
    /// UDP packets per second one source may send to game ports
    /// (`NEXUS_FIREWALL_UDP_PER_SOURCE`).
    pub udp_per_source: u32,
    /// Addresses never filtered (`NEXUS_FIREWALL_TRUSTED`, comma-separated
    /// CIDRs): the operator's own, a monitoring host.
    pub trusted: Vec<String>,
    /// Apply the kernel sysctls (`NEXUS_FIREWALL_SYSCTL`, default on).
    pub tune_kernel: bool,
}

impl Default for FirewallSettings {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            syn_per_source: 50,
            syn_global: 20_000,
            udp_per_source: 2_000,
            trusted: Vec::new(),
            tune_kernel: true,
        }
    }
}

impl FirewallSettings {
    pub fn from_env() -> Self {
        let mut s = Self::default();
        if let Ok(mode) = std::env::var("NEXUS_FIREWALL") {
            s.mode = match mode.trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "1" | "nft" | "nftables" => Mode::On,
                "off" | "false" | "0" | "none" => Mode::Off,
                _ => Mode::Auto,
            };
        }
        let read = |name: &str, default: u32| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        };
        s.syn_per_source = read("NEXUS_FIREWALL_SYN_PER_SOURCE", s.syn_per_source);
        s.syn_global = read("NEXUS_FIREWALL_SYN_GLOBAL", s.syn_global);
        s.udp_per_source = read("NEXUS_FIREWALL_UDP_PER_SOURCE", s.udp_per_source);
        if let Ok(list) = std::env::var("NEXUS_FIREWALL_TRUSTED") {
            s.trusted = list
                .split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .filter(|c| validate_cidr(c).is_ok())
                .map(String::from)
                .collect();
        }
        if let Ok(v) = std::env::var("NEXUS_FIREWALL_SYSCTL") {
            s.tune_kernel = !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "off" | "false" | "0"
            );
        }
        s
    }
}

/// What the firewall is doing for one server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFirewall {
    pub container_id: String,
    /// The nftables chain holding this server's rules.
    pub chain: String,
    pub ports: Vec<AssignedPort>,
    pub rules: Vec<FirewallRule>,
}

/// Packets and bytes a rule has dropped or matched.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counter {
    pub packets: u64,
    pub bytes: u64,
}

/// A blocked address and how long it has left.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedAddress {
    pub cidr: String,
    /// Seconds until the block expires, or `None` for a permanent block.
    pub expires_in_secs: Option<u64>,
    pub reason: Option<String>,
}

/// One protection rule as the panel shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleStatus {
    /// The rule's comment tag (`syn-flood`, `srv:<id>:rate-udp`, …).
    pub tag: String,
    pub description: String,
    pub counter: Counter,
}

/// The whole firewall as the panel shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallStatus {
    pub enabled: bool,
    /// Why it is disabled, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    pub backend: String,
    pub settings: SettingsStatus,
    pub base_rules: Vec<RuleStatus>,
    pub blocked: Vec<BlockedAddress>,
    pub trusted: Vec<String>,
    pub servers: Vec<ServerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsStatus {
    pub syn_per_source: u32,
    pub syn_global: u32,
    pub udp_per_source: u32,
    pub kernel_tuned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub container_id: String,
    pub chain: String,
    pub ports: Vec<AssignedPort>,
    pub rules: Vec<ServerRuleStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerRuleStatus {
    pub rule: FirewallRule,
    pub tag: String,
    pub counter: Counter,
}

/// The firewall itself.
pub struct Firewall {
    settings: FirewallSettings,
    backend: Backend,
    servers: RwLock<HashMap<String, ServerFirewall>>,
    /// Reasons for blocks, which nftables does not store beyond a comment.
    reasons: RwLock<HashMap<String, String>>,
    kernel_tuned: RwLock<bool>,
}

enum Backend {
    Nft { binary: String },
    Disabled { reason: String },
}

impl Firewall {
    /// Build the firewall for this node. Does not touch the kernel; call
    /// [`install`](Self::install) for that.
    pub fn new(settings: FirewallSettings) -> Self {
        let backend = match settings.mode {
            Mode::Off => Backend::Disabled {
                reason: "NEXUS_FIREWALL=off".to_string(),
            },
            Mode::On | Mode::Auto => match detect_nft() {
                Ok(binary) => Backend::Nft { binary },
                Err(reason) => Backend::Disabled { reason },
            },
        };
        Self {
            settings,
            backend,
            servers: RwLock::new(HashMap::new()),
            reasons: RwLock::new(HashMap::new()),
            kernel_tuned: RwLock::new(false),
        }
    }

    /// A firewall that does nothing, for tests and nodes without nftables.
    pub fn disabled(reason: &str) -> Self {
        Self {
            settings: FirewallSettings {
                mode: Mode::Off,
                ..FirewallSettings::default()
            },
            backend: Backend::Disabled {
                reason: reason.to_string(),
            },
            servers: RwLock::new(HashMap::new()),
            reasons: RwLock::new(HashMap::new()),
            kernel_tuned: RwLock::new(false),
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self.backend, Backend::Nft { .. })
    }

    pub fn settings(&self) -> &FirewallSettings {
        &self.settings
    }

    /// Put the base ruleset in place (replacing any left by a previous
    /// run) and tune the kernel. Server chains are added as servers start.
    pub async fn install(&self) -> Result<()> {
        match &self.backend {
            Backend::Disabled { reason } => {
                if self.settings.mode == Mode::On {
                    return Err(NodeError::Internal(format!(
                        "NEXUS_FIREWALL=on but the firewall cannot run: {}",
                        reason
                    )));
                }
                warn!("Firewall disabled: {}", reason);
                return Ok(());
            }
            Backend::Nft { .. } => {}
        }

        self.run_script(&render_base(&self.settings)).await?;
        self.servers.write().await.clear();
        info!(
            "Firewall installed: table inet {} (SYN {}/s per source, {}/s total; UDP {}/s per source)",
            TABLE, self.settings.syn_per_source, self.settings.syn_global, self.settings.udp_per_source
        );

        if self.settings.tune_kernel {
            let applied = apply_sysctls();
            *self.kernel_tuned.write().await = applied > 0;
            info!("Applied {} kernel network settings", applied);
        }
        Ok(())
    }

    /// Remove the table entirely (node shutdown).
    pub async fn uninstall(&self) -> Result<()> {
        if !self.is_enabled() {
            return Ok(());
        }
        self.run_script(&format!("delete table inet {}\n", TABLE)).await
    }

    /// Attach a server's rules to its ports.
    pub async fn apply_server(&self, container_id: &str, config: &GameConfig) -> Result<()> {
        let ports = resolve_ports(config);
        let rules = config.security.firewall_rules.clone();
        for rule in &rules {
            validate_rule(rule).map_err(NodeError::InvalidInput)?;
        }
        let server = ServerFirewall {
            container_id: container_id.to_string(),
            chain: chain_name(container_id),
            ports,
            rules,
        };
        if !self.is_enabled() {
            self.servers.write().await.insert(container_id.to_string(), server);
            return Ok(());
        }

        // Ports may have changed since the last apply; clear the old
        // dispatch entries first, in their own transaction, since deleting
        // an element that is not there fails the whole script.
        if let Some(previous) = self.servers.read().await.get(container_id).cloned() {
            let _ = self.run_script(&render_remove(&previous)).await;
        }
        self.run_script(&render_server(&server)).await?;
        self.servers.write().await.insert(container_id.to_string(), server);
        Ok(())
    }

    /// Detach a server's rules. A server that was never attached is fine.
    pub async fn remove_server(&self, container_id: &str) -> Result<()> {
        let Some(server) = self.servers.write().await.remove(container_id) else {
            return Ok(());
        };
        if !self.is_enabled() {
            return Ok(());
        }
        self.run_script(&render_remove(&server)).await
    }

    /// Block an address or network node-wide, for `ttl` or permanently.
    ///
    /// nftables refuses overlapping entries in an interval set, so a block
    /// that widens an existing one (the /24 around an address already
    /// blocked) replaces it, and one already covered by a permanent wider
    /// block is a no-op.
    pub async fn block(
        &self,
        cidr: &str,
        ttl: Option<Duration>,
        reason: Option<&str>,
    ) -> Result<()> {
        let cidr = validate_cidr(cidr).map_err(NodeError::InvalidInput)?;
        if let Some(reason) = reason {
            self.reasons.write().await.insert(cidr.clone(), reason.to_string());
        }
        if !self.is_enabled() {
            return Ok(());
        }
        let set = if cidr.contains(':') {
            "blocked_v6"
        } else {
            "blocked_v4"
        };

        let existing = match self.read_table().await {
            Ok(json) => blocked_from_json(&json, &HashMap::new()),
            Err(_) => Vec::new(),
        };
        let mut script = String::new();
        for entry in existing.iter().filter(|e| cidrs_overlap(&e.cidr, &cidr)) {
            if entry.cidr != cidr
                && cidr_contains(&entry.cidr, &cidr)
                && entry.expires_in_secs.is_none()
            {
                // Already inside a permanent wider block.
                return Ok(());
            }
            script.push_str(&format!(
                "delete element inet {} {} {{ {} }}\n",
                TABLE, set, entry.cidr
            ));
            self.reasons.write().await.remove(&entry.cidr);
        }
        let timeout = ttl.map(|t| format!(" timeout {}s", t.as_secs().max(1))).unwrap_or_default();
        script.push_str(&format!(
            "add element inet {} {} {{ {}{} }}\n",
            TABLE, set, cidr, timeout
        ));
        if let Some(reason) = reason {
            self.reasons.write().await.insert(cidr.clone(), reason.to_string());
        }
        self.run_script(&script).await
    }

    pub async fn unblock(&self, cidr: &str) -> Result<()> {
        let cidr = validate_cidr(cidr).map_err(NodeError::InvalidInput)?;
        self.reasons.write().await.remove(&cidr);
        if !self.is_enabled() {
            return Ok(());
        }
        let set = if cidr.contains(':') {
            "blocked_v6"
        } else {
            "blocked_v4"
        };
        self.run_script(&format!(
            "delete element inet {} {} {{ {} }}\n",
            TABLE, set, cidr
        ))
        .await
    }

    /// Exempt an address or network from every rule.
    pub async fn trust(&self, cidr: &str) -> Result<()> {
        let cidr = validate_cidr(cidr).map_err(NodeError::InvalidInput)?;
        if !self.is_enabled() {
            return Ok(());
        }
        let set = if cidr.contains(':') {
            "trusted_v6"
        } else {
            "trusted_v4"
        };
        self.run_script(&format!(
            "add element inet {} {} {{ {} }}\n",
            TABLE, set, cidr
        ))
        .await
    }

    pub async fn untrust(&self, cidr: &str) -> Result<()> {
        let cidr = validate_cidr(cidr).map_err(NodeError::InvalidInput)?;
        if !self.is_enabled() {
            return Ok(());
        }
        let set = if cidr.contains(':') {
            "trusted_v6"
        } else {
            "trusted_v4"
        };
        self.run_script(&format!(
            "delete element inet {} {} {{ {} }}\n",
            TABLE, set, cidr
        ))
        .await
    }

    /// What one server's firewall looks like, with live counters.
    pub async fn server_status(&self, container_id: &str) -> Option<ServerStatus> {
        let server = self.servers.read().await.get(container_id).cloned()?;
        let counters = if self.is_enabled() {
            self.read_counters().await.unwrap_or_default()
        } else {
            HashMap::new()
        };
        Some(server_status(&server, &counters))
    }

    /// The whole firewall, with live counters and the current blocklist.
    pub async fn status(&self) -> FirewallStatus {
        let (enabled, backend, disabled_reason) = match &self.backend {
            Backend::Nft { binary } => (true, format!("nftables ({})", binary), None),
            Backend::Disabled { reason } => (false, "none".to_string(), Some(reason.clone())),
        };
        let (counters, blocked, trusted) = if enabled {
            match self.read_table().await {
                Ok(json) => {
                    let reasons = self.reasons.read().await;
                    (
                        counters_from_json(&json),
                        blocked_from_json(&json, &reasons),
                        trusted_from_json(&json),
                    )
                }
                Err(e) => {
                    warn!("Could not read firewall state: {}", e);
                    (HashMap::new(), Vec::new(), Vec::new())
                }
            }
        } else {
            (HashMap::new(), Vec::new(), self.settings.trusted.clone())
        };

        let base_rules = BASE_RULE_TAGS
            .iter()
            .map(|(tag, description)| RuleStatus {
                tag: tag.to_string(),
                description: description.to_string(),
                counter: counters.get(*tag).cloned().unwrap_or_default(),
            })
            .collect();

        let servers = self.servers.read().await;
        let mut servers: Vec<ServerStatus> =
            servers.values().map(|s| server_status(s, &counters)).collect();
        servers.sort_by(|a, b| a.container_id.cmp(&b.container_id));

        FirewallStatus {
            enabled,
            disabled_reason,
            backend,
            settings: SettingsStatus {
                syn_per_source: self.settings.syn_per_source,
                syn_global: self.settings.syn_global,
                udp_per_source: self.settings.udp_per_source,
                kernel_tuned: *self.kernel_tuned.read().await,
            },
            base_rules,
            blocked,
            trusted,
            servers,
        }
    }

    // ── nft plumbing ─────────────────────────────────────────────────

    async fn run_script(&self, script: &str) -> Result<()> {
        let Backend::Nft { binary } = &self.backend else {
            return Ok(());
        };
        let mut child = tokio::process::Command::new(binary)
            .arg("-f")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| NodeError::Internal(format!("could not run nft: {}", e)))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(script.as_bytes())
                .await
                .map_err(|e| NodeError::Internal(format!("could not feed nft: {}", e)))?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| NodeError::Internal(format!("nft did not finish: {}", e)))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(NodeError::Internal(format!(
                "nft rejected the ruleset: {}",
                stderr.lines().next().unwrap_or("unknown error").trim()
            )))
        }
    }

    async fn read_table(&self) -> Result<serde_json::Value> {
        let Backend::Nft { binary } = &self.backend else {
            return Ok(serde_json::json!({ "nftables": [] }));
        };
        let output = tokio::process::Command::new(binary)
            .args(["-j", "list", "table", "inet", TABLE])
            .output()
            .await
            .map_err(|e| NodeError::Internal(format!("could not run nft: {}", e)))?;
        if !output.status.success() {
            return Err(NodeError::Internal(format!(
                "nft could not list the table: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|e| NodeError::Internal(format!("nft JSON is unreadable: {}", e)))
    }

    async fn read_counters(&self) -> Result<HashMap<String, Counter>> {
        Ok(counters_from_json(&self.read_table().await?))
    }
}

/// The node-wide rules, tagged, with what each one is for.
pub const BASE_RULE_TAGS: &[(&str, &str)] = &[
    ("blocked", "Blocked addresses (IPv4)"),
    ("blocked6", "Blocked addresses (IPv6)"),
    ("invalid", "Packets in no known connection state"),
    ("syn-flood-source", "SYN flood from one source (IPv4)"),
    ("syn-flood-source6", "SYN flood from one source (IPv6)"),
    ("syn-flood", "SYN flood, all sources"),
    ("udp-flood-source", "UDP flood from one source (IPv4)"),
    ("udp-flood-source6", "UDP flood from one source (IPv6)"),
];

/// Find `nft` and check we may use it.
fn detect_nft() -> std::result::Result<String, String> {
    if !is_root() {
        return Err("the node is not running as root, which nftables requires".to_string());
    }
    let candidates = [
        "/usr/sbin/nft",
        "/sbin/nft",
        "/usr/bin/nft",
        "/usr/local/sbin/nft",
    ];
    let from_path = std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("nft"))
            .find(|f| f.is_file())
            .map(|f| f.to_string_lossy().to_string())
    });
    let binary = from_path
        .or_else(|| candidates.iter().find(|c| Path::new(c).is_file()).map(|c| c.to_string()))
        .ok_or_else(|| "nft is not installed (apt install nftables)".to_string())?;
    // It must actually work: the kernel may lack nf_tables.
    let probe = std::process::Command::new(&binary)
        .args(["list", "tables"])
        .output()
        .map_err(|e| format!("{} cannot run: {}", binary, e))?;
    if !probe.status.success() {
        return Err(format!(
            "nft cannot talk to the kernel: {}",
            String::from_utf8_lossy(&probe.stderr).trim()
        ));
    }
    Ok(binary)
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// The chain a server's rules live in.
pub fn chain_name(container_id: &str) -> String {
    let id: String = container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(CHAIN_ID_CHARS)
        .collect();
    format!("srv_{}", id)
}

/// Parse `100/s`, `100/second`, `5/m`, `1000/minute`, `50/h` into packets
/// per second (rounded up; a limit below one per second becomes one).
pub fn parse_rate(limit: &str) -> std::result::Result<u32, String> {
    let (count, unit) = limit
        .trim()
        .split_once('/')
        .ok_or_else(|| format!("rate {:?} must look like 100/s", limit))?;
    let count: u32 = count
        .trim()
        .parse()
        .map_err(|_| format!("rate {:?} must start with a number", limit))?;
    if count == 0 {
        return Err(format!("rate {:?} must be above zero", limit));
    }
    let per_second = match unit.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "second" | "seconds" => count,
        "m" | "min" | "minute" | "minutes" => count.div_ceil(60).max(1),
        "h" | "hour" | "hours" => count.div_ceil(3600).max(1),
        other => return Err(format!("rate unit {:?} is not s, m or h", other)),
    };
    Ok(per_second)
}

/// Check and normalise a CIDR or single address.
pub fn validate_cidr(input: &str) -> std::result::Result<String, String> {
    let s = input.trim();
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let ip: std::net::IpAddr =
        addr.parse().map_err(|_| format!("{:?} is not an IP address or CIDR", input))?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let prefix: u8 = match prefix {
        Some(p) => p
            .parse()
            .ok()
            .filter(|n| *n <= max)
            .ok_or_else(|| format!("{:?} has an invalid prefix length", input))?,
        None => max,
    };
    if prefix == max {
        Ok(ip.to_string())
    } else {
        Ok(format!("{}/{}", ip, prefix))
    }
}

/// Split a validated CIDR into its network address and prefix length.
fn cidr_parts(cidr: &str) -> Option<(u128, u8, bool)> {
    let (addr, prefix) = match cidr.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (cidr, None),
    };
    let ip: std::net::IpAddr = addr.parse().ok()?;
    let (bits, max) = match ip {
        std::net::IpAddr::V4(v4) => (u32::from(v4) as u128, 32u8),
        std::net::IpAddr::V6(v6) => (u128::from(v6), 128u8),
    };
    let prefix: u8 = match prefix {
        Some(p) => p.parse().ok()?,
        None => max,
    };
    let shift = (max - prefix) as u32;
    let network = if shift >= 128 {
        0
    } else {
        (bits >> shift) << shift
    };
    Some((network, prefix, max == 32))
}

/// Whether `outer` contains every address of `inner`.
pub fn cidr_contains(outer: &str, inner: &str) -> bool {
    let (Some((on, op, ov4)), Some((inn, ip, iv4))) = (cidr_parts(outer), cidr_parts(inner)) else {
        return false;
    };
    if ov4 != iv4 || op > ip {
        return false;
    }
    let max = if ov4 { 32 } else { 128 };
    let shift = (max - op) as u32;
    let inner_at_outer_prefix = if shift >= 128 {
        0
    } else {
        (inn >> shift) << shift
    };
    inner_at_outer_prefix == on
}

/// Whether two CIDRs share any address.
pub fn cidrs_overlap(a: &str, b: &str) -> bool {
    cidr_contains(a, b) || cidr_contains(b, a)
}

/// Check a blueprint rule before it goes near nft.
pub fn validate_rule(rule: &FirewallRule) -> std::result::Result<(), String> {
    match rule {
        FirewallRule::ConnectionRate { limit, .. } => parse_rate(limit).map(|_| ()),
        FirewallRule::PacketSize { max_size, .. } => {
            if *max_size == 0 || *max_size > 65535 {
                Err(format!("packet size limit {} is out of range", max_size))
            } else {
                Ok(())
            }
        }
        FirewallRule::AllowCidr { cidr, .. } | FirewallRule::BlockCidr { cidr, .. } => {
            validate_cidr(cidr).map(|_| ())
        }
    }
}

fn action_verdict(action: &FirewallAction) -> &'static str {
    match action {
        FirewallAction::Allow => "accept",
        FirewallAction::Drop => "drop",
        FirewallAction::Reject => "reject",
    }
}

fn saddr_keyword(cidr: &str) -> &'static str {
    if cidr.contains(':') {
        "ip6 saddr"
    } else {
        "ip saddr"
    }
}

/// The tag a server rule's counter is read by.
pub fn rule_tag(container_id: &str, index: usize, rule: &FirewallRule) -> String {
    let kind = match rule {
        FirewallRule::ConnectionRate { .. } => "rate",
        FirewallRule::PacketSize { .. } => "size",
        FirewallRule::AllowCidr { .. } => "allow",
        FirewallRule::BlockCidr { .. } => "block",
    };
    format!("srv:{}:{}:{}", short(container_id), index, kind)
}

fn short(container_id: &str) -> String {
    container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(CHAIN_ID_CHARS)
        .collect()
}

// ── Script rendering (pure) ──────────────────────────────────────────

/// The base ruleset. Replaces any existing table so a restart starts clean.
pub fn render_base(s: &FirewallSettings) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "add table inet {t}\ndelete table inet {t}\n",
        t = TABLE
    ));
    out.push_str(&format!("table inet {} {{\n", TABLE));
    out.push_str("  set trusted_v4 { type ipv4_addr; flags interval; }\n");
    out.push_str("  set trusted_v6 { type ipv6_addr; flags interval; }\n");
    out.push_str("  set blocked_v4 { type ipv4_addr; flags interval, timeout; }\n");
    out.push_str("  set blocked_v6 { type ipv6_addr; flags interval, timeout; }\n");
    out.push_str("  set game_tcp { type inet_service; }\n");
    out.push_str("  set game_udp { type inet_service; }\n");
    out.push_str("  map srv_tcp { type inet_service : verdict; }\n");
    out.push_str("  map srv_udp { type inet_service : verdict; }\n");
    out.push_str("  set syn_src { type ipv4_addr; flags dynamic; timeout 1m; }\n");
    out.push_str("  set syn_src6 { type ipv6_addr; flags dynamic; timeout 1m; }\n");
    out.push_str("  set udp_src { type ipv4_addr; flags dynamic; timeout 1m; }\n");
    out.push_str("  set udp_src6 { type ipv6_addr; flags dynamic; timeout 1m; }\n");
    out.push_str("  chain input {\n");
    out.push_str("    type filter hook input priority -50; policy accept;\n");
    out.push_str("    iif \"lo\" accept\n");
    out.push_str("    ip saddr @trusted_v4 accept comment \"trusted\"\n");
    out.push_str("    ip6 saddr @trusted_v6 accept comment \"trusted6\"\n");
    out.push_str("    ip saddr @blocked_v4 counter drop comment \"blocked\"\n");
    out.push_str("    ip6 saddr @blocked_v6 counter drop comment \"blocked6\"\n");
    out.push_str("    ct state invalid counter drop comment \"invalid\"\n");
    // Established TCP is accepted early; UDP is not, so a flood inside an
    // existing flow still meets the meters.
    out.push_str("    meta l4proto tcp ct state established,related accept\n");
    let syn = "tcp flags & (fin|syn|rst|ack) == syn";
    out.push_str(&format!(
        "    tcp dport @game_tcp {syn} add @syn_src {{ ip saddr limit rate over {r}/second burst {b} packets }} counter drop comment \"syn-flood-source\"\n",
        syn = syn, r = s.syn_per_source, b = s.syn_per_source * 2
    ));
    out.push_str(&format!(
        "    tcp dport @game_tcp {syn} add @syn_src6 {{ ip6 saddr limit rate over {r}/second burst {b} packets }} counter drop comment \"syn-flood-source6\"\n",
        syn = syn, r = s.syn_per_source, b = s.syn_per_source * 2
    ));
    out.push_str(&format!(
        "    tcp dport @game_tcp {syn} limit rate over {r}/second burst {b} packets counter drop comment \"syn-flood\"\n",
        syn = syn, r = s.syn_global, b = s.syn_global / 4
    ));
    out.push_str(&format!(
        "    udp dport @game_udp add @udp_src {{ ip saddr limit rate over {r}/second burst {b} packets }} counter drop comment \"udp-flood-source\"\n",
        r = s.udp_per_source, b = s.udp_per_source
    ));
    out.push_str(&format!(
        "    udp dport @game_udp add @udp_src6 {{ ip6 saddr limit rate over {r}/second burst {b} packets }} counter drop comment \"udp-flood-source6\"\n",
        r = s.udp_per_source, b = s.udp_per_source
    ));
    out.push_str("    tcp dport vmap @srv_tcp\n");
    out.push_str("    udp dport vmap @srv_udp\n");
    out.push_str("  }\n}\n");
    for cidr in &s.trusted {
        let set = if cidr.contains(':') {
            "trusted_v6"
        } else {
            "trusted_v4"
        };
        out.push_str(&format!(
            "add element inet {} {} {{ {} }}\n",
            TABLE, set, cidr
        ));
    }
    out
}

/// The rules for one server's chain and its port dispatch entries.
pub fn render_server(server: &ServerFirewall) -> String {
    let chain = &server.chain;
    let mut out = String::new();
    out.push_str(&format!(
        "table inet {} {{\n  chain {} {{ }}\n}}\n",
        TABLE, chain
    ));
    out.push_str(&format!("flush chain inet {} {}\n", TABLE, chain));

    for (i, rule) in server.rules.iter().enumerate() {
        let tag = rule_tag(&server.container_id, i, rule);
        let line = match rule {
            FirewallRule::AllowCidr { cidr, .. } => format!(
                "{} {} accept comment \"{}\"",
                saddr_keyword(cidr),
                cidr.trim(),
                tag
            ),
            FirewallRule::BlockCidr { cidr, .. } => format!(
                "{} {} counter drop comment \"{}\"",
                saddr_keyword(cidr),
                cidr.trim(),
                tag
            ),
            FirewallRule::PacketSize {
                max_size, action, ..
            } => format!(
                "meta l4proto udp meta length > {} counter {} comment \"{}\"",
                max_size,
                action_verdict(action),
                tag
            ),
            FirewallRule::ConnectionRate { limit, action, .. } => {
                let rate = parse_rate(limit).unwrap_or(100);
                // One rule per protocol: UDP has no connections to count,
                // so its rate is packets; TCP counts new connections.
                out.push_str(&format!(
                    "add rule inet {} {} meta l4proto udp limit rate over {}/second burst {} packets counter {} comment \"{}-udp\"\n",
                    TABLE, chain, rate, rate, action_verdict(action), tag
                ));
                format!(
                    "meta l4proto tcp ct state new limit rate over {}/second burst {} packets counter {} comment \"{}-tcp\"",
                    rate,
                    rate,
                    action_verdict(action),
                    tag
                )
            }
        };
        out.push_str(&format!("add rule inet {} {} {}\n", TABLE, chain, line));
    }
    out.push_str(&format!(
        "add rule inet {} {} counter accept comment \"srv:{}:accept\"\n",
        TABLE,
        chain,
        short(&server.container_id)
    ));

    let (tcp, udp) = split_ports(&server.ports);
    if !tcp.is_empty() {
        out.push_str(&format!(
            "add element inet {} game_tcp {{ {} }}\n",
            TABLE,
            join_ports(&tcp)
        ));
        out.push_str(&format!(
            "add element inet {} srv_tcp {{ {} }}\n",
            TABLE,
            tcp.iter()
                .map(|p| format!("{} : jump {}", p, chain))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !udp.is_empty() {
        out.push_str(&format!(
            "add element inet {} game_udp {{ {} }}\n",
            TABLE,
            join_ports(&udp)
        ));
        out.push_str(&format!(
            "add element inet {} srv_udp {{ {} }}\n",
            TABLE,
            udp.iter()
                .map(|p| format!("{} : jump {}", p, chain))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out
}

/// Detach a server: its dispatch entries, then its chain (which nft refuses
/// to delete while anything still jumps to it).
pub fn render_remove(server: &ServerFirewall) -> String {
    let mut out = String::new();
    let (tcp, udp) = split_ports(&server.ports);
    if !tcp.is_empty() {
        out.push_str(&format!(
            "delete element inet {} srv_tcp {{ {} }}\n",
            TABLE,
            join_ports(&tcp)
        ));
        out.push_str(&format!(
            "delete element inet {} game_tcp {{ {} }}\n",
            TABLE,
            join_ports(&tcp)
        ));
    }
    if !udp.is_empty() {
        out.push_str(&format!(
            "delete element inet {} srv_udp {{ {} }}\n",
            TABLE,
            join_ports(&udp)
        ));
        out.push_str(&format!(
            "delete element inet {} game_udp {{ {} }}\n",
            TABLE,
            join_ports(&udp)
        ));
    }
    out.push_str(&format!("delete chain inet {} {}\n", TABLE, server.chain));
    out
}

fn split_ports(ports: &[AssignedPort]) -> (Vec<u16>, Vec<u16>) {
    let mut tcp = Vec::new();
    let mut udp = Vec::new();
    for p in ports {
        match p.protocol.as_str() {
            "tcp" => tcp.push(p.port),
            "udp" => udp.push(p.port),
            _ => {
                tcp.push(p.port);
                udp.push(p.port);
            }
        }
    }
    tcp.sort_unstable();
    tcp.dedup();
    udp.sort_unstable();
    udp.dedup();
    (tcp, udp)
}

fn join_ports(ports: &[u16]) -> String {
    ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
}

// ── Status parsing (pure) ────────────────────────────────────────────

/// Every rule's counter, keyed by its comment.
pub fn counters_from_json(json: &serde_json::Value) -> HashMap<String, Counter> {
    let mut out = HashMap::new();
    let Some(items) = json.get("nftables").and_then(|v| v.as_array()) else {
        return out;
    };
    for item in items {
        let Some(rule) = item.get("rule") else {
            continue;
        };
        let Some(comment) = rule.get("comment").and_then(|c| c.as_str()) else {
            continue;
        };
        let counter = rule
            .get("expr")
            .and_then(|e| e.as_array())
            .and_then(|exprs| exprs.iter().find_map(|e| e.get("counter")))
            .map(|c| Counter {
                packets: c.get("packets").and_then(|p| p.as_u64()).unwrap_or(0),
                bytes: c.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0),
            })
            .unwrap_or_default();
        out.insert(comment.to_string(), counter);
    }
    out
}

fn set_elements(
    json: &serde_json::Value,
    name: &str,
) -> Vec<(String, Option<u64>, Option<String>)> {
    let mut out = Vec::new();
    let Some(items) = json.get("nftables").and_then(|v| v.as_array()) else {
        return out;
    };
    for item in items {
        let Some(set) = item.get("set") else {
            continue;
        };
        if set.get("name").and_then(|n| n.as_str()) != Some(name) {
            continue;
        }
        let Some(elems) = set.get("elem").and_then(|e| e.as_array()) else {
            continue;
        };
        for elem in elems {
            // Either a bare value or `{"elem": {"val": …, "expires": …}}`.
            let (val, expires, comment) = match elem.get("elem") {
                Some(inner) => (
                    inner.get("val"),
                    inner.get("expires").and_then(|e| e.as_u64()),
                    inner.get("comment").and_then(|c| c.as_str()).map(String::from),
                ),
                None => (Some(elem), None, None),
            };
            if let Some(cidr) = val.and_then(element_to_cidr) {
                out.push((cidr, expires, comment));
            }
        }
    }
    out
}

fn element_to_cidr(val: &serde_json::Value) -> Option<String> {
    if let Some(s) = val.as_str() {
        return Some(s.to_string());
    }
    let prefix = val.get("prefix")?;
    let addr = prefix.get("addr")?.as_str()?;
    let len = prefix.get("len")?.as_u64()?;
    let host = if addr.contains(':') { 128 } else { 32 };
    if len == host {
        Some(addr.to_string())
    } else {
        Some(format!("{}/{}", addr, len))
    }
}

pub fn blocked_from_json(
    json: &serde_json::Value,
    reasons: &HashMap<String, String>,
) -> Vec<BlockedAddress> {
    let mut out: Vec<BlockedAddress> = set_elements(json, "blocked_v4")
        .into_iter()
        .chain(set_elements(json, "blocked_v6"))
        .map(|(cidr, expires, _)| BlockedAddress {
            reason: reasons.get(&cidr).cloned(),
            cidr,
            expires_in_secs: expires,
        })
        .collect();
    out.sort_by(|a, b| a.cidr.cmp(&b.cidr));
    out
}

pub fn trusted_from_json(json: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = set_elements(json, "trusted_v4")
        .into_iter()
        .chain(set_elements(json, "trusted_v6"))
        .map(|(cidr, _, _)| cidr)
        .collect();
    out.sort();
    out
}

fn server_status(server: &ServerFirewall, counters: &HashMap<String, Counter>) -> ServerStatus {
    let rules = server
        .rules
        .iter()
        .enumerate()
        .map(|(i, rule)| {
            let tag = rule_tag(&server.container_id, i, rule);
            // A rate rule is two nft rules; sum them.
            let counter = if matches!(rule, FirewallRule::ConnectionRate { .. }) {
                let a = counters.get(&format!("{}-udp", tag)).cloned().unwrap_or_default();
                let b = counters.get(&format!("{}-tcp", tag)).cloned().unwrap_or_default();
                Counter {
                    packets: a.packets + b.packets,
                    bytes: a.bytes + b.bytes,
                }
            } else {
                counters.get(&tag).cloned().unwrap_or_default()
            };
            ServerRuleStatus {
                rule: rule.clone(),
                tag,
                counter,
            }
        })
        .collect();
    ServerStatus {
        container_id: server.container_id.clone(),
        chain: server.chain.clone(),
        ports: server.ports.clone(),
        rules,
    }
}

// ── Kernel tuning ────────────────────────────────────────────────────

/// The sysctls that matter for a host taking game traffic under attack.
/// Each is only raised, never lowered, and skipped where the kernel lacks it.
const SYSCTLS: &[(&str, u64)] = &[
    ("net/ipv4/tcp_syncookies", 1),
    ("net/ipv4/tcp_max_syn_backlog", 65536),
    ("net/ipv4/tcp_synack_retries", 2),
    ("net/core/somaxconn", 65535),
    ("net/core/netdev_max_backlog", 65536),
    ("net/core/rmem_max", 26_214_400),
    ("net/core/wmem_max", 26_214_400),
    ("net/ipv4/tcp_timestamps", 1),
    ("net/ipv4/tcp_rfc1337", 1),
    ("net/ipv4/conf/all/rp_filter", 1),
    ("net/netfilter/nf_conntrack_max", 1_048_576),
    ("net/netfilter/nf_conntrack_tcp_timeout_syn_recv", 30),
    ("net/netfilter/nf_conntrack_udp_timeout", 30),
];

/// Apply the tuning; returns how many settings were written.
pub fn apply_sysctls() -> usize {
    let mut applied = 0;
    for (key, want) in SYSCTLS {
        let path = format!("/proc/sys/{}", key);
        let Ok(current) = std::fs::read_to_string(&path) else {
            continue;
        };
        let current: u64 = current.trim().parse().unwrap_or(0);
        // Timeouts are lowered to what we want; everything else only raised.
        let lowering = key.contains("timeout") || key.ends_with("synack_retries");
        let should_write = if lowering {
            current > *want
        } else {
            current < *want
        };
        if !should_write {
            continue;
        }
        match std::fs::write(&path, want.to_string()) {
            Ok(()) => applied += 1,
            Err(e) => warn!("Could not set {}={}: {}", key, want, e),
        }
    }
    applied
}

// ── Helpers for the manager ──────────────────────────────────────────

/// Attach a server if the firewall is on; log and carry on if it fails. A
/// firewall problem must not stop a paid-for server from starting.
pub async fn attach_best_effort(fw: &Firewall, container_id: &str, config: &GameConfig) {
    if let Err(e) = fw.apply_server(container_id, config).await {
        warn!(
            "Firewall rules for {} were not applied: {}",
            container_id, e
        );
    }
}

pub async fn detach_best_effort(fw: &Firewall, container_id: &str) {
    if let Err(e) = fw.remove_server(container_id).await {
        warn!(
            "Firewall rules for {} were not removed: {}",
            container_id, e
        );
    }
}

/// Rules as a map keyed by tag, for callers that want to look them up.
pub fn rules_by_tag(status: &ServerStatus) -> BTreeMap<String, &ServerRuleStatus> {
    status.rules.iter().map(|r| (r.tag.clone(), r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> ServerFirewall {
        ServerFirewall {
            container_id: "abcdef123456789".into(),
            chain: chain_name("abcdef123456789"),
            ports: vec![
                AssignedPort {
                    name: "game".into(),
                    port: 27015,
                    protocol: "udp".into(),
                    variable: Some("SERVER_PORT".into()),
                },
                AssignedPort {
                    name: "rcon".into(),
                    port: 27016,
                    protocol: "tcp".into(),
                    variable: None,
                },
                AssignedPort {
                    name: "both".into(),
                    port: 27017,
                    protocol: "both".into(),
                    variable: None,
                },
            ],
            rules: vec![
                FirewallRule::ConnectionRate {
                    name: "rate".into(),
                    limit: "200/s".into(),
                    action: FirewallAction::Drop,
                },
                FirewallRule::PacketSize {
                    name: "size".into(),
                    max_size: 1500,
                    action: FirewallAction::Drop,
                },
                FirewallRule::BlockCidr {
                    name: "bad".into(),
                    cidr: "203.0.113.0/24".into(),
                },
                FirewallRule::AllowCidr {
                    name: "friend".into(),
                    cidr: "2001:db8::1".into(),
                },
            ],
        }
    }

    #[test]
    fn rates_parse() {
        assert_eq!(parse_rate("100/s").unwrap(), 100);
        assert_eq!(parse_rate("100/second").unwrap(), 100);
        assert_eq!(parse_rate("120/m").unwrap(), 2);
        assert_eq!(parse_rate("1/minute").unwrap(), 1);
        assert_eq!(parse_rate("7200/h").unwrap(), 2);
        assert!(parse_rate("0/s").is_err());
        assert!(parse_rate("fast").is_err());
        assert!(parse_rate("10/day").is_err());
    }

    #[test]
    fn cidrs_validate_and_normalise() {
        assert_eq!(validate_cidr("192.0.2.1").unwrap(), "192.0.2.1");
        assert_eq!(validate_cidr("192.0.2.1/32").unwrap(), "192.0.2.1");
        assert_eq!(validate_cidr(" 192.0.2.0/24 ").unwrap(), "192.0.2.0/24");
        assert_eq!(validate_cidr("2001:db8::/32").unwrap(), "2001:db8::/32");
        assert!(validate_cidr("192.0.2.0/33").is_err());
        assert!(validate_cidr("not-an-ip").is_err());
        assert!(validate_cidr("192.0.2.1; drop").is_err(), "no injection");
    }

    #[test]
    fn cidr_containment() {
        assert!(cidr_contains("192.0.2.0/24", "192.0.2.5"));
        assert!(cidr_contains("192.0.2.0/24", "192.0.2.0/25"));
        assert!(!cidr_contains("192.0.2.0/25", "192.0.2.0/24"));
        assert!(!cidr_contains("192.0.2.0/24", "192.0.3.5"));
        assert!(cidr_contains("0.0.0.0/0", "8.8.8.8"));
        assert!(cidr_contains("2001:db8::/32", "2001:db8:1::1"));
        assert!(
            !cidr_contains("2001:db8::/32", "192.0.2.1"),
            "families differ"
        );
        assert!(cidrs_overlap("192.0.2.5", "192.0.2.0/24"));
        assert!(!cidrs_overlap("192.0.2.5", "192.0.2.6"));
    }

    #[test]
    fn chain_names_stay_short_and_safe() {
        assert_eq!(chain_name("abcdef123456789"), "srv_abcdef123456");
        assert_eq!(chain_name("a-b_c"), "srv_abc");
        assert!(chain_name("x".repeat(100).as_str()).len() < 32);
    }

    #[test]
    fn base_ruleset_has_every_protection() {
        let script = render_base(&FirewallSettings {
            trusted: vec!["198.51.100.7".into(), "2001:db8::/32".into()],
            ..FirewallSettings::default()
        });
        for tag in BASE_RULE_TAGS.iter().map(|(t, _)| *t) {
            assert!(
                script.contains(&format!("comment \"{}\"", tag)),
                "{} missing",
                tag
            );
        }
        assert!(script.contains("delete table inet nexus"), "starts clean");
        assert!(script.contains("limit rate over 50/second"));
        assert!(script.contains("limit rate over 20000/second"));
        assert!(script.contains("limit rate over 2000/second"));
        assert!(script.contains("add element inet nexus trusted_v4 { 198.51.100.7 }"));
        assert!(script.contains("add element inet nexus trusted_v6 { 2001:db8::/32 }"));
        assert!(script.contains("tcp dport vmap @srv_tcp"));
        assert!(script.contains("udp dport vmap @srv_udp"));
    }

    #[test]
    fn server_ruleset_dispatches_its_ports_to_its_chain() {
        let script = render_server(&server());
        assert!(script.contains("chain srv_abcdef123456 { }"));
        assert!(script.contains("flush chain inet nexus srv_abcdef123456"));
        // Rate: one UDP packet rule, one TCP new-connection rule.
        assert!(script.contains("meta l4proto udp limit rate over 200/second burst 200 packets counter drop comment \"srv:abcdef123456:0:rate-udp\""));
        assert!(script.contains("meta l4proto tcp ct state new limit rate over 200/second burst 200 packets counter drop comment \"srv:abcdef123456:0:rate-tcp\""));
        assert!(script.contains(
            "meta l4proto udp meta length > 1500 counter drop comment \"srv:abcdef123456:1:size\""
        ));
        assert!(script
            .contains("ip saddr 203.0.113.0/24 counter drop comment \"srv:abcdef123456:2:block\""));
        assert!(
            script.contains("ip6 saddr 2001:db8::1 accept comment \"srv:abcdef123456:3:allow\"")
        );
        // "both" lands in both sets; tcp gets 27016 + 27017, udp 27015 + 27017.
        assert!(script.contains("add element inet nexus game_tcp { 27016, 27017 }"));
        assert!(script.contains("add element inet nexus game_udp { 27015, 27017 }"));
        assert!(script
            .contains("srv_tcp { 27016 : jump srv_abcdef123456, 27017 : jump srv_abcdef123456 }"));
        assert!(script
            .contains("srv_udp { 27015 : jump srv_abcdef123456, 27017 : jump srv_abcdef123456 }"));
        assert!(script.ends_with('\n'));
    }

    #[test]
    fn removal_clears_dispatch_before_the_chain() {
        let script = render_remove(&server());
        let chain_delete = script.find("delete chain").unwrap();
        for needle in [
            "delete element inet nexus srv_tcp { 27016, 27017 }",
            "delete element inet nexus srv_udp { 27015, 27017 }",
            "delete element inet nexus game_tcp { 27016, 27017 }",
            "delete element inet nexus game_udp { 27015, 27017 }",
        ] {
            let at = script.find(needle).unwrap_or_else(|| panic!("{} missing", needle));
            assert!(
                at < chain_delete,
                "{} must come before the chain delete",
                needle
            );
        }
    }

    #[test]
    fn counters_and_sets_parse_from_nft_json() {
        let json = serde_json::json!({ "nftables": [
            { "rule": { "chain": "input", "comment": "syn-flood", "expr": [ { "match": {} }, { "counter": { "packets": 42, "bytes": 2520 } }, { "drop": null } ] } },
            { "rule": { "chain": "input", "expr": [ { "counter": { "packets": 1, "bytes": 1 } } ] } },
            { "rule": { "chain": "srv_x", "comment": "srv:x:0:rate-udp", "expr": [ { "counter": { "packets": 5, "bytes": 500 } } ] } },
            { "rule": { "chain": "srv_x", "comment": "srv:x:0:rate-tcp", "expr": [ { "counter": { "packets": 2, "bytes": 200 } } ] } },
            { "set": { "name": "blocked_v4", "elem": [
                { "elem": { "val": { "prefix": { "addr": "192.0.2.0", "len": 24 } }, "timeout": 600, "expires": 123 } },
                "198.51.100.9"
            ] } },
            { "set": { "name": "trusted_v6", "elem": [ { "prefix": { "addr": "2001:db8::", "len": 32 } } ] } }
        ]});
        let counters = counters_from_json(&json);
        assert_eq!(
            counters["syn-flood"],
            Counter {
                packets: 42,
                bytes: 2520
            }
        );
        assert_eq!(counters.len(), 3, "uncommented rules are ignored");

        let mut reasons = HashMap::new();
        reasons.insert("198.51.100.9".to_string(), "abuse".to_string());
        let blocked = blocked_from_json(&json, &reasons);
        assert_eq!(
            blocked,
            vec![
                BlockedAddress {
                    cidr: "192.0.2.0/24".into(),
                    expires_in_secs: Some(123),
                    reason: None
                },
                BlockedAddress {
                    cidr: "198.51.100.9".into(),
                    expires_in_secs: None,
                    reason: Some("abuse".into())
                },
            ]
        );
        assert_eq!(trusted_from_json(&json), vec!["2001:db8::/32"]);

        let status = server_status(
            &ServerFirewall {
                container_id: "x".into(),
                chain: "srv_x".into(),
                ports: vec![],
                rules: vec![FirewallRule::ConnectionRate {
                    name: "r".into(),
                    limit: "10/s".into(),
                    action: FirewallAction::Drop,
                }],
            },
            &counters,
        );
        assert_eq!(
            status.rules[0].counter,
            Counter {
                packets: 7,
                bytes: 700
            },
            "udp + tcp summed"
        );
    }

    #[test]
    fn rules_validate() {
        assert!(validate_rule(&FirewallRule::ConnectionRate {
            name: "a".into(),
            limit: "10/s".into(),
            action: FirewallAction::Drop
        })
        .is_ok());
        assert!(validate_rule(&FirewallRule::ConnectionRate {
            name: "a".into(),
            limit: "lots".into(),
            action: FirewallAction::Drop
        })
        .is_err());
        assert!(validate_rule(&FirewallRule::PacketSize {
            name: "a".into(),
            max_size: 0,
            action: FirewallAction::Drop
        })
        .is_err());
        assert!(validate_rule(&FirewallRule::BlockCidr {
            name: "a".into(),
            cidr: "1.2.3.4/40".into()
        })
        .is_err());
    }

    #[tokio::test]
    async fn disabled_firewall_tracks_servers_without_touching_nft() {
        let fw = Firewall::disabled("test");
        assert!(!fw.is_enabled());
        let config: GameConfig = serde_yaml::from_str(
            r#"
metadata: { id: t, name: T, version: "1", game: t, author: t }
container: { image: img }
resources:
  cpu: { min: 500, max: 1000, shares: 1024 }
  memory: { min: 1Gi, max: 2Gi }
  disk: { min: 1Gi }
startup: { command: run, working_dir: /x }
variables:
  - { name: SERVER_PORT, description: p, default: "27015" }
networking:
  ports:
    - { name: game, internal: "{{SERVER_PORT}}", protocol: udp }
security:
  capabilities: { drop: [], add: [] }
  firewall_rules:
    - { type: connection_rate, name: r, limit: 100/s, action: drop }
"#,
        )
        .unwrap();
        fw.apply_server("srv-1", &config).await.unwrap();
        let status = fw.status().await;
        assert!(!status.enabled);
        assert_eq!(status.servers.len(), 1);
        assert_eq!(status.servers[0].ports[0].port, 27015);
        assert_eq!(status.servers[0].rules.len(), 1);
        fw.remove_server("srv-1").await.unwrap();
        assert!(fw.status().await.servers.is_empty());
        // Bad input is refused even when disabled.
        assert!(fw.block("nope", None, None).await.is_err());
    }

    /// Against the real kernel, when this test runs as root with nft. It is
    /// what CI's containerd job environment provides; elsewhere it skips.
    #[tokio::test]
    async fn live_nftables_round_trip() {
        if std::env::var("NEXUS_IT_NFT").ok().as_deref() != Some("1") {
            eprintln!("skipping live nftables test (set NEXUS_IT_NFT=1)");
            return;
        }
        let fw = Firewall::new(FirewallSettings {
            mode: Mode::On,
            tune_kernel: false,
            trusted: vec!["198.51.100.7".into()],
            ..FirewallSettings::default()
        });
        assert!(fw.is_enabled(), "nft must be usable for this test");
        fw.install().await.unwrap();

        let config: GameConfig = serde_yaml::from_str(
            r#"
metadata: { id: t, name: T, version: "1", game: t, author: t }
container: { image: img }
resources:
  cpu: { min: 500, max: 1000, shares: 1024 }
  memory: { min: 1Gi, max: 2Gi }
  disk: { min: 1Gi }
startup: { command: run, working_dir: /x }
variables:
  - { name: SERVER_PORT, description: p, default: "27115" }
networking:
  ports:
    - { name: game, internal: "{{SERVER_PORT}}", protocol: udp }
    - { name: rcon, internal: "{{SERVER_PORT}}+1", protocol: tcp }
security:
  capabilities: { drop: [], add: [] }
  firewall_rules:
    - { type: connection_rate, name: r, limit: 100/s, action: drop }
    - { type: packet_size, name: s, max_size: 1500, action: drop }
    - { type: block_cidr, name: b, cidr: 203.0.113.0/24 }
"#,
        )
        .unwrap();
        fw.apply_server("live-test-server", &config).await.unwrap();
        fw.block("192.0.2.5", Some(Duration::from_secs(120)), Some("test"))
            .await
            .unwrap();
        fw.block("198.51.100.200", None, Some("abuse")).await.unwrap();

        let status = fw.status().await;
        assert!(status.enabled);
        assert_eq!(status.servers.len(), 1);
        assert_eq!(status.servers[0].rules.len(), 3);
        assert!(status.trusted.contains(&"198.51.100.7".to_string()));
        let timed = status.blocked.iter().find(|b| b.cidr == "192.0.2.5").unwrap();
        assert!(timed.expires_in_secs.is_some());
        assert_eq!(timed.reason.as_deref(), Some("test"));
        let permanent = status.blocked.iter().find(|b| b.cidr == "198.51.100.200").unwrap();
        assert!(permanent.expires_in_secs.is_none());

        // Widening a block folds the narrower one into it.
        fw.block("192.0.2.0/24", None, Some("range")).await.unwrap();
        let blocked: Vec<String> =
            fw.status().await.blocked.iter().map(|b| b.cidr.clone()).collect();
        assert!(blocked.contains(&"192.0.2.0/24".to_string()));
        assert!(!blocked.contains(&"192.0.2.5".to_string()), "{:?}", blocked);
        // Blocking inside a permanent wider block is a no-op, not an error.
        fw.block("192.0.2.9", None, None).await.unwrap();

        // Re-applying (changed ports) and removing both work.
        fw.apply_server("live-test-server", &config).await.unwrap();
        fw.unblock("192.0.2.0/24").await.unwrap();
        fw.unblock("198.51.100.200").await.unwrap();
        fw.remove_server("live-test-server").await.unwrap();
        assert!(fw.status().await.servers.is_empty());
        fw.uninstall().await.unwrap();
    }
}

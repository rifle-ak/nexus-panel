//! Provisioning: creating servers on behalf of a billing system.
//!
//! A billing system (WHMCS, via the module in `whmcs/`) does not know YAML.
//! It knows a product — a blueprint id, a memory size, a slot count — and a
//! service id it will retry with until the panel says the server exists.
//! This module turns that into a blueprint the container manager can run:
//!
//! - **Overrides.** Resource limits and variable values are applied on top of
//!   a shipped (or supplied) blueprint, so a product is "Minecraft Paper with
//!   6 GiB" rather than a hand-maintained copy of the whole file.
//! - **Port allocation.** Servers share the host's network namespace, so two
//!   servers on one node must not bind the same port. Every port a blueprint
//!   declares through a `{{VARIABLE}}` template is assigned a free value from
//!   the node's provisioning range; ports the blueprint pins to a literal
//!   number are checked for conflicts and refused rather than left to fail at
//!   start.
//! - **Records.** Each provisioned server keeps a small record — the external
//!   id it was created for, the ports it was given, the resources it was
//!   sold with — under the data directory, so a billing system's retry after
//!   a timeout finds the server it already has instead of making a second.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nexus_config::{GameConfig, Protocol};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

/// Subdirectory of `DATA_DIR` holding provisioning records.
const RECORD_SUBDIR: &str = ".nexus/provision";

/// Ports handed out when `PROVISION_PORT_RANGE` is unset. Above the
/// registered range, below the ephemeral range Linux uses for outbound
/// connections (32768+), and clear of every default a shipped blueprint pins.
pub const DEFAULT_PORT_RANGE: (u16, u16) = (20000, 29999);

/// Longest an external id may be. WHMCS service ids are integers; this leaves
/// room for a prefix and for other billing systems.
const MAX_EXTERNAL_ID_LEN: usize = 128;
const MAX_NAME_LEN: usize = 128;
const MAX_VARIABLE_VALUE_LEN: usize = 4096;
/// Least memory a game server can be provisioned with. Below this the
/// container's own runtime will not start, let alone a game.
const MIN_MEMORY_MB: u32 = 256;

/// Node-level provisioning settings, from the environment.
#[derive(Debug, Clone)]
pub struct ProvisionSettings {
    /// Inclusive range ports are allocated from.
    pub port_range: (u16, u16),
    /// The address customers connect to. The node cannot reliably discover
    /// this itself (it may be behind NAT or a DDoS scrubbing proxy), so it is
    /// configured; when unset, the billing system falls back to the address
    /// it has on file for the node.
    pub public_ip: Option<String>,
}

impl Default for ProvisionSettings {
    fn default() -> Self {
        Self {
            port_range: DEFAULT_PORT_RANGE,
            public_ip: None,
        }
    }
}

impl ProvisionSettings {
    pub fn from_env() -> Self {
        let mut settings = Self::default();
        if let Ok(raw) = std::env::var("PROVISION_PORT_RANGE") {
            match parse_port_range(&raw) {
                Some(range) => settings.port_range = range,
                None => warn!(
                    "Ignoring invalid PROVISION_PORT_RANGE {:?}; expected START-END, using {}-{}",
                    raw, DEFAULT_PORT_RANGE.0, DEFAULT_PORT_RANGE.1
                ),
            }
        }
        settings.public_ip = std::env::var("NODE_PUBLIC_IP")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        settings
    }
}

/// Parse `START-END` into an inclusive port range.
pub fn parse_port_range(raw: &str) -> Option<(u16, u16)> {
    let (start, end) = raw.trim().split_once('-')?;
    let start: u16 = start.trim().parse().ok()?;
    let end: u16 = end.trim().parse().ok()?;
    if start == 0 || start > end {
        return None;
    }
    Some((start, end))
}

/// What a provisioned server was sold with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionResources {
    pub memory_mb: u32,
    pub cpu_millicores: u32,
    pub disk_mb: u32,
}

/// A port a server was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignedPort {
    /// The blueprint's name for it (`game`, `query`, `rcon`).
    pub name: String,
    pub port: u16,
    /// `tcp`, `udp` or `both`.
    pub protocol: String,
    /// The blueprint variable this port was written into, if it came from a
    /// template rather than a literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
}

/// The durable record of a provisioned server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionRecord {
    pub container_id: String,
    /// The billing system's id for the service this server belongs to.
    pub external_id: String,
    pub name: String,
    /// Shipped blueprint id, or `custom` when YAML was supplied.
    pub blueprint: String,
    pub game: String,
    /// Opaque owner reference (a client id or email) for the operator's
    /// benefit when looking at the node; nothing here keys on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub resources: ProvisionResources,
    pub ports: Vec<AssignedPort>,
    /// Variable values the billing system set, so a package change can
    /// re-apply them on top of the blueprint.
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl ProvisionRecord {
    /// The port customers connect to: the first port the blueprint declares.
    pub fn primary_port(&self) -> Option<u16> {
        self.ports.first().map(|p| p.port)
    }
}

/// Persistent store of provisioning records, keyed by container id.
pub struct ProvisionStore {
    dir: PathBuf,
    records: RwLock<HashMap<String, ProvisionRecord>>,
    /// Serialises allocate-then-create so two concurrent provisioning calls
    /// cannot both be handed the same port.
    pub lock: Mutex<()>,
}

impl ProvisionStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(RECORD_SUBDIR),
            records: RwLock::new(HashMap::new()),
            lock: Mutex::new(()),
        }
    }

    fn path(&self, container_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", container_id))
    }

    /// Read every record on disk. Unreadable files are logged and skipped:
    /// one corrupt record must not take the whole node's provisioning down.
    pub async fn load(&self) -> usize {
        let Ok(mut entries) = tokio::fs::read_dir(&self.dir).await else {
            return 0;
        };
        let mut loaded = HashMap::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => match serde_json::from_slice::<ProvisionRecord>(&bytes) {
                    Ok(record) => {
                        loaded.insert(record.container_id.clone(), record);
                    }
                    Err(e) => warn!("Provisioning record {:?} is unreadable: {}", path, e),
                },
                Err(e) => warn!("Provisioning record {:?} is unreadable: {}", path, e),
            }
        }
        let count = loaded.len();
        *self.records.write().await = loaded;
        count
    }

    pub async fn get(&self, container_id: &str) -> Option<ProvisionRecord> {
        self.records.read().await.get(container_id).cloned()
    }

    pub async fn find_by_external_id(&self, external_id: &str) -> Option<ProvisionRecord> {
        self.records
            .read()
            .await
            .values()
            .find(|r| r.external_id == external_id)
            .cloned()
    }

    pub async fn list(&self) -> Vec<ProvisionRecord> {
        let mut all: Vec<_> = self.records.read().await.values().cloned().collect();
        all.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        all
    }

    /// Insert or replace a record, writing it to disk first.
    pub async fn save(&self, record: ProvisionRecord) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Write-then-rename so a crash mid-write cannot leave a half record.
        let final_path = self.path(&record.container_id);
        let tmp_path = self.dir.join(format!(".{}.json.tmp", record.container_id));
        tokio::fs::write(&tmp_path, bytes).await?;
        tokio::fs::rename(&tmp_path, &final_path).await?;
        self.records.write().await.insert(record.container_id.clone(), record);
        Ok(())
    }

    pub async fn remove(&self, container_id: &str) -> Option<ProvisionRecord> {
        let removed = self.records.write().await.remove(container_id);
        if removed.is_some() {
            let _ = tokio::fs::remove_file(self.path(container_id)).await;
        }
        removed
    }
}

/// Why a provisioning request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionError {
    /// The request itself is wrong (bad id, bad variable name, …).
    Invalid(String),
    /// A port the server needs is already taken on this node.
    PortConflict(String),
    /// The node's port range has no room for another server of this shape.
    NoPortsAvailable(String),
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProvisionError::Invalid(m)
            | ProvisionError::PortConflict(m)
            | ProvisionError::NoPortsAvailable(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ProvisionError {}

/// What a billing product changes about a blueprint.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub name: Option<String>,
    pub memory_mb: Option<u32>,
    pub cpu_millicores: Option<u32>,
    pub disk_mb: Option<u32>,
    pub variables: BTreeMap<String, String>,
}

/// Validate a billing system's id for a service.
pub fn validate_external_id(id: &str) -> Result<(), ProvisionError> {
    if id.is_empty() || id.len() > MAX_EXTERNAL_ID_LEN {
        return Err(ProvisionError::Invalid(format!(
            "external_id must be 1–{} characters",
            MAX_EXTERNAL_ID_LEN
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(ProvisionError::Invalid(
            "external_id may contain only letters, digits, '-', '_', '.' and ':'".into(),
        ));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ProvisionError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_NAME_LEN {
        return Err(ProvisionError::Invalid(format!(
            "name must be 1–{} characters",
            MAX_NAME_LEN
        )));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(ProvisionError::Invalid(
            "name may not contain control characters".into(),
        ));
    }
    Ok(())
}

fn validate_variable(name: &str, value: &str) -> Result<(), ProvisionError> {
    let mut chars = name.chars();
    let head_ok = chars.next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false);
    if !head_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ProvisionError::Invalid(format!(
            "variable name {:?} is not a valid environment variable name",
            name
        )));
    }
    if value.len() > MAX_VARIABLE_VALUE_LEN {
        return Err(ProvisionError::Invalid(format!(
            "variable {} is longer than {} bytes",
            name, MAX_VARIABLE_VALUE_LEN
        )));
    }
    if value.contains(['\0', '\n', '\r']) {
        return Err(ProvisionError::Invalid(format!(
            "variable {} may not contain newlines or NUL",
            name
        )));
    }
    Ok(())
}

/// Apply a product's overrides to a blueprint.
///
/// Resources become hard values: a sold 4 GiB server gets `min = max = 4Gi`
/// rather than inheriting a blueprint's "4 to 8" range, because the customer
/// paid for exactly one of those numbers. Variables the blueprint declares
/// have their default replaced; unknown ones become plain environment.
pub fn apply_overrides(config: &mut GameConfig, o: &Overrides) -> Result<(), ProvisionError> {
    if let Some(name) = &o.name {
        validate_name(name)?;
        config.metadata.name = name.trim().to_string();
    }

    if let Some(mb) = o.memory_mb {
        if mb < MIN_MEMORY_MB {
            return Err(ProvisionError::Invalid(format!(
                "memory_mb must be at least {}",
                MIN_MEMORY_MB
            )));
        }
        let mem = format!("{}Mi", mb);
        config.resources.memory.min = mem.clone();
        config.resources.memory.max = mem;
        // Swap on a sold server is a way to quietly deliver less than the
        // memory paid for; take it out.
        config.resources.memory.swap = None;

        // Java games size their heap from a MEMORY variable, not from the
        // container limit. Leave headroom for the JVM's own overhead so the
        // container's limit is never the thing that kills the server.
        if !o.variables.contains_key("MEMORY") {
            if let Some(var) = config.variables.iter_mut().find(|v| v.name == "MEMORY") {
                var.default = format!("{}M", mb.saturating_mul(3) / 4);
            }
        }
    }

    if let Some(millicores) = o.cpu_millicores {
        if millicores < 100 {
            return Err(ProvisionError::Invalid(
                "cpu_millicores must be at least 100 (0.1 cores)".into(),
            ));
        }
        config.resources.cpu.min = millicores;
        config.resources.cpu.max = millicores;
        // cgroup shares are relative weights; scale them with the sold CPU so
        // a 4-core product wins contention against a 1-core one.
        config.resources.cpu.shares = (millicores.saturating_mul(1024) / 1000).clamp(2, 262_144);
    }

    if let Some(mb) = o.disk_mb {
        if mb == 0 {
            return Err(ProvisionError::Invalid("disk_mb must be positive".into()));
        }
        config.resources.disk.min = format!("{}Mi", mb);
    }

    for (name, value) in &o.variables {
        validate_variable(name, value)?;
        match config.variables.iter_mut().find(|v| &v.name == name) {
            Some(var) => {
                // The blueprint says what this variable may hold.
                var.validate_value(value).map_err(ProvisionError::Invalid)?;
                var.default = value.clone();
            }
            None => {
                config.container.environment.insert(name.clone(), value.clone());
            }
        }
    }

    Ok(())
}

/// How a blueprint expresses one port.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PortRef {
    /// A fixed number the blueprint pins.
    Literal(u16),
    /// `{{VAR}}` or `{{VAR}}+n`: whatever the variable holds, plus an offset.
    Template { variable: String, offset: u16 },
}

/// Parse a blueprint port's `internal` field.
fn parse_port_ref(internal: &str) -> Option<PortRef> {
    let s = internal.trim();
    if let Ok(port) = s.parse::<u16>() {
        return (port > 0).then_some(PortRef::Literal(port));
    }
    let rest = s.strip_prefix("{{")?;
    let (variable, tail) = rest.split_once("}}")?;
    let variable = variable.trim();
    if variable.is_empty() {
        return None;
    }
    let tail = tail.trim();
    let offset = if tail.is_empty() {
        0
    } else {
        tail.strip_prefix('+')?.trim().parse::<u16>().ok()?
    };
    Some(PortRef::Template {
        variable: variable.to_string(),
        offset,
    })
}

fn protocol_name(p: &Protocol) -> &'static str {
    match p {
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
        Protocol::Both => "both",
    }
}

/// The ports a blueprint will bind with its variables as they stand.
///
/// This is how an existing server's ports are read back: from its stored
/// blueprint, whose variable defaults were set when it was provisioned (or
/// are the shipped defaults, for servers created by hand).
pub fn resolve_ports(config: &GameConfig) -> Vec<AssignedPort> {
    let vars: HashMap<&str, &str> =
        config.variables.iter().map(|v| (v.name.as_str(), v.default.as_str())).collect();

    config
        .networking
        .ports
        .iter()
        .filter_map(|port| {
            let (value, variable) = match parse_port_ref(&port.internal)? {
                PortRef::Literal(p) => (p, None),
                PortRef::Template { variable, offset } => {
                    let base: u16 = vars.get(variable.as_str())?.trim().parse().ok()?;
                    (base.checked_add(offset)?, Some(variable))
                }
            };
            Some(AssignedPort {
                name: port.name.clone(),
                port: value,
                protocol: protocol_name(&port.protocol).to_string(),
                variable,
            })
        })
        .collect()
}

/// Assign free ports to a blueprint's templated ports and write them into
/// its variables. Literal ports are checked against `used` and refused on a
/// conflict, since a server that cannot bind at start is not provisioned.
///
/// `requested` pins the first templated variable to a specific port (a
/// customer keeping the address they had); it is refused if not free.
pub fn allocate_ports(
    config: &mut GameConfig,
    used: &HashSet<u16>,
    range: (u16, u16),
    requested: Option<u16>,
) -> Result<Vec<AssignedPort>, ProvisionError> {
    // Templated variables in order of first appearance, each with the widest
    // offset any port applies to it: `{{SERVER_PORT}}+2` means the variable
    // needs three consecutive free ports.
    let mut variables: Vec<(String, u16)> = Vec::new();
    for port in &config.networking.ports {
        match parse_port_ref(&port.internal) {
            Some(PortRef::Literal(p)) => {
                if used.contains(&p) {
                    return Err(ProvisionError::PortConflict(format!(
                        "this blueprint pins port {} ({}), which another server on this node already uses",
                        p, port.name
                    )));
                }
            }
            Some(PortRef::Template { variable, offset }) => {
                match variables.iter_mut().find(|(v, _)| *v == variable) {
                    Some((_, span)) => *span = (*span).max(offset + 1),
                    None => variables.push((variable, offset + 1)),
                }
            }
            None => {
                return Err(ProvisionError::Invalid(format!(
                    "port {} has an internal value {:?} that is neither a number nor a {{{{VARIABLE}}}} template",
                    port.name, port.internal
                )))
            }
        }
    }

    // Literal ports also count as taken by this server: a templated port
    // must not land on one of them.
    let mut taken: HashSet<u16> = used.clone();
    for port in &config.networking.ports {
        if let Some(PortRef::Literal(p)) = parse_port_ref(&port.internal) {
            taken.insert(p);
        }
    }

    let total_span: u32 = variables.iter().map(|(_, span)| *span as u32).sum();
    if total_span > 0 {
        let fits = |base: u16| -> bool {
            let end = base as u32 + total_span - 1;
            if end > range.1 as u32 || end > u16::MAX as u32 {
                return false;
            }
            (base as u32..=end).all(|p| !taken.contains(&(p as u16)))
        };

        let base = match requested {
            Some(port) => {
                if port < range.0 || port > range.1 {
                    return Err(ProvisionError::Invalid(format!(
                        "requested port {} is outside this node's provisioning range {}-{}",
                        port, range.0, range.1
                    )));
                }
                if !fits(port) {
                    return Err(ProvisionError::PortConflict(format!(
                        "requested port {} (or one of the {} ports after it this game needs) is already in use",
                        port,
                        total_span - 1
                    )));
                }
                port
            }
            None => (range.0..=range.1).find(|&b| fits(b)).ok_or_else(|| {
                ProvisionError::NoPortsAvailable(format!(
                    "no block of {} free ports left in {}-{}; widen PROVISION_PORT_RANGE or add a node",
                    total_span, range.0, range.1
                ))
            })?,
        };

        let mut next = base;
        for (variable, span) in &variables {
            match config.variables.iter_mut().find(|v| &v.name == variable) {
                Some(var) => var.default = next.to_string(),
                None => {
                    // A port template with no declared variable: the game
                    // still reads it from the environment.
                    config.container.environment.insert(variable.clone(), next.to_string());
                }
            }
            next += span;
        }
    }

    Ok(resolve_ports(config))
}

/// Total size of everything under `path`, following no symlinks.
///
/// Blocking; run it on a blocking thread. A big Rust or Arma server has
/// hundreds of thousands of files.
pub fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

pub fn now_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blueprint(yaml: &str) -> GameConfig {
        serde_yaml::from_str(yaml).expect("test blueprint parses")
    }

    const MINIMAL: &str = r#"
metadata:
  id: t
  name: Test
  version: "1"
  game: test
  author: t
container:
  image: img
resources:
  cpu: { min: 1000, max: 2000, shares: 1024 }
  memory: { min: 1Gi, max: 2Gi }
  disk: { min: 5Gi }
startup:
  command: run
  working_dir: /home/container
variables:
  - { name: SERVER_PORT, description: p, default: "25565" }
  - { name: RCON_PORT, description: p, default: "25575" }
  - { name: MEMORY, description: m, default: "1G" }
networking:
  ports:
    - { name: game, internal: "{{SERVER_PORT}}", protocol: tcp }
    - { name: game-extra, internal: "{{SERVER_PORT}}+2", protocol: udp }
    - { name: rcon, internal: "{{RCON_PORT}}", protocol: tcp }
security:
  capabilities: { drop: [], add: [] }
"#;

    #[test]
    fn port_range_parsing() {
        assert_eq!(parse_port_range("20000-29999"), Some((20000, 29999)));
        assert_eq!(parse_port_range(" 100 - 200 "), Some((100, 200)));
        assert_eq!(parse_port_range("5-5"), Some((5, 5)));
        assert_eq!(parse_port_range("300-200"), None);
        assert_eq!(parse_port_range("0-10"), None);
        assert_eq!(parse_port_range("abc"), None);
        assert_eq!(parse_port_range("70000-70001"), None);
    }

    #[test]
    fn port_ref_parsing() {
        assert_eq!(parse_port_ref("27015"), Some(PortRef::Literal(27015)));
        assert_eq!(
            parse_port_ref("{{SERVER_PORT}}"),
            Some(PortRef::Template {
                variable: "SERVER_PORT".into(),
                offset: 0
            })
        );
        assert_eq!(
            parse_port_ref("{{ SERVER_PORT }}+2"),
            Some(PortRef::Template {
                variable: "SERVER_PORT".into(),
                offset: 2
            })
        );
        assert_eq!(parse_port_ref("0"), None);
        assert_eq!(parse_port_ref("{{}}"), None);
        assert_eq!(parse_port_ref("{{X}}-1"), None);
        assert_eq!(parse_port_ref("garbage"), None);
    }

    #[test]
    fn allocation_gives_each_variable_a_contiguous_block() {
        let mut config = blueprint(MINIMAL);
        let used = HashSet::new();
        let ports = allocate_ports(&mut config, &used, (20000, 20010), None).unwrap();

        // SERVER_PORT needs 3 ports (offset +2), so RCON_PORT starts after.
        let var = |n: &str| config.variables.iter().find(|v| v.name == n).unwrap().default.clone();
        assert_eq!(var("SERVER_PORT"), "20000");
        assert_eq!(var("RCON_PORT"), "20003");

        let numbers: Vec<u16> = ports.iter().map(|p| p.port).collect();
        assert_eq!(numbers, vec![20000, 20002, 20003]);
        assert_eq!(ports[0].variable.as_deref(), Some("SERVER_PORT"));
        assert_eq!(ports[1].protocol, "udp");
    }

    #[test]
    fn allocation_skips_ports_in_use() {
        let mut config = blueprint(MINIMAL);
        // 20001 is taken, so the 3-wide block cannot start at 20000.
        let used: HashSet<u16> = [20001].into_iter().collect();
        let ports = allocate_ports(&mut config, &used, (20000, 20010), None).unwrap();
        assert_eq!(ports[0].port, 20002);
        assert_eq!(ports[2].port, 20005);
    }

    #[test]
    fn allocation_honours_a_requested_port() {
        let mut config = blueprint(MINIMAL);
        let used = HashSet::new();
        let ports = allocate_ports(&mut config, &used, (20000, 20100), Some(20050)).unwrap();
        assert_eq!(ports[0].port, 20050);

        let taken: HashSet<u16> = [20051].into_iter().collect();
        let err = allocate_ports(&mut blueprint(MINIMAL), &taken, (20000, 20100), Some(20050))
            .unwrap_err();
        assert!(matches!(err, ProvisionError::PortConflict(_)));

        let err =
            allocate_ports(&mut blueprint(MINIMAL), &used, (20000, 20100), Some(9)).unwrap_err();
        assert!(matches!(err, ProvisionError::Invalid(_)));
    }

    #[test]
    fn allocation_fails_when_the_range_is_full() {
        let mut config = blueprint(MINIMAL);
        let used = HashSet::new();
        // Needs 4 ports; only 3 available.
        let err = allocate_ports(&mut config, &used, (20000, 20002), None).unwrap_err();
        assert!(matches!(err, ProvisionError::NoPortsAvailable(_)));
    }

    #[test]
    fn literal_ports_conflict_instead_of_failing_at_start() {
        let yaml = MINIMAL.replace(r#"internal: "{{RCON_PORT}}""#, r#"internal: "27016""#);
        let mut config = blueprint(&yaml);
        let used: HashSet<u16> = [27016].into_iter().collect();
        let err = allocate_ports(&mut config, &used, (20000, 20100), None).unwrap_err();
        assert!(matches!(err, ProvisionError::PortConflict(_)));

        // And a templated port never lands on this server's own literal.
        let mut config = blueprint(&yaml.replace("27016", "20001"));
        let ports = allocate_ports(&mut config, &HashSet::new(), (20000, 20100), None).unwrap();
        // SERVER_PORT needs 20000..=20002 which overlaps the literal 20001.
        assert_eq!(ports[0].port, 20002);
    }

    #[test]
    fn resolve_reads_ports_back_from_variable_defaults() {
        let config = blueprint(MINIMAL);
        let ports = resolve_ports(&config);
        let numbers: Vec<u16> = ports.iter().map(|p| p.port).collect();
        assert_eq!(numbers, vec![25565, 25567, 25575]);
    }

    #[test]
    fn every_shipped_blueprint_can_be_allocated_ports() {
        // The blueprints the panel ships must all be provisionable, and the
        // allocator must recognise every port expression they use.
        for entry in
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../blueprints")).unwrap()
        {
            let path = entry.unwrap().path();
            let yaml = std::fs::read_to_string(&path).unwrap();
            let mut config = blueprint(&yaml);
            let ports = allocate_ports(&mut config, &HashSet::new(), DEFAULT_PORT_RANGE, None)
                .unwrap_or_else(|e| panic!("{:?}: {}", path, e));
            assert!(!ports.is_empty(), "{:?} declares no ports", path);
            let templated = ports.iter().filter(|p| p.variable.is_some()).count();
            assert!(templated > 0, "{:?} has no templated port", path);
            for p in ports.iter().filter(|p| p.variable.is_some()) {
                assert!(
                    (DEFAULT_PORT_RANGE.0..=DEFAULT_PORT_RANGE.1).contains(&p.port),
                    "{:?}: {} allocated outside the range",
                    path,
                    p.port
                );
            }
        }
    }

    #[test]
    fn overrides_pin_resources_and_size_the_jvm_heap() {
        let mut config = blueprint(MINIMAL);
        let o = Overrides {
            name: Some("  Customer's server ".into()),
            memory_mb: Some(4096),
            cpu_millicores: Some(2500),
            disk_mb: Some(20480),
            variables: BTreeMap::new(),
        };
        apply_overrides(&mut config, &o).unwrap();
        assert_eq!(config.metadata.name, "Customer's server");
        assert_eq!(config.resources.memory.min, "4096Mi");
        assert_eq!(config.resources.memory.max, "4096Mi");
        assert_eq!(config.resources.memory.swap, None);
        assert_eq!(config.resources.cpu.min, 2500);
        assert_eq!(config.resources.cpu.max, 2500);
        assert_eq!(config.resources.cpu.shares, 2560);
        assert_eq!(config.resources.disk.min, "20480Mi");
        let mem = config.variables.iter().find(|v| v.name == "MEMORY").unwrap();
        assert_eq!(mem.default, "3072M");
        // The result still validates as a blueprint.
        config.validate().unwrap();
    }

    #[test]
    fn overrides_set_declared_variables_and_add_unknown_ones() {
        let mut config = blueprint(MINIMAL);
        let mut variables = BTreeMap::new();
        variables.insert("MEMORY".to_string(), "2G".to_string());
        variables.insert("MAX_PLAYERS".to_string(), "20".to_string());
        let o = Overrides {
            memory_mb: Some(4096),
            variables,
            ..Default::default()
        };
        apply_overrides(&mut config, &o).unwrap();
        // An explicit MEMORY wins over the derived one.
        let mem = config.variables.iter().find(|v| v.name == "MEMORY").unwrap();
        assert_eq!(mem.default, "2G");
        assert_eq!(
            config.container.environment.get("MAX_PLAYERS").map(String::as_str),
            Some("20")
        );
    }

    #[test]
    fn overrides_reject_bad_input() {
        let bad = |o: Overrides| apply_overrides(&mut blueprint(MINIMAL), &o).unwrap_err();
        assert!(matches!(
            bad(Overrides {
                memory_mb: Some(64),
                ..Default::default()
            }),
            ProvisionError::Invalid(_)
        ));
        assert!(matches!(
            bad(Overrides {
                cpu_millicores: Some(10),
                ..Default::default()
            }),
            ProvisionError::Invalid(_)
        ));
        assert!(matches!(
            bad(Overrides {
                name: Some("".into()),
                ..Default::default()
            }),
            ProvisionError::Invalid(_)
        ));
        let mut variables = BTreeMap::new();
        variables.insert("bad-name".to_string(), "x".to_string());
        assert!(matches!(
            bad(Overrides {
                variables,
                ..Default::default()
            }),
            ProvisionError::Invalid(_)
        ));
        let mut variables = BTreeMap::new();
        variables.insert("OK".to_string(), "line1\nline2".to_string());
        assert!(matches!(
            bad(Overrides {
                variables,
                ..Default::default()
            }),
            ProvisionError::Invalid(_)
        ));
    }

    #[test]
    fn overrides_enforce_the_blueprints_variable_rules() {
        let yaml = MINIMAL.replace(
            r#"  - { name: MEMORY, description: m, default: "1G" }"#,
            r#"  - { name: MEMORY, description: m, default: "1G" }
  - { name: MAX_PLAYERS, description: p, default: "20", rules: [{ type: numeric, min: 1, max: 200 }] }"#,
        );
        let mut variables = BTreeMap::new();
        variables.insert("MAX_PLAYERS".to_string(), "-1".to_string());
        let err = apply_overrides(
            &mut blueprint(&yaml),
            &Overrides {
                variables,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, ProvisionError::Invalid(ref m) if m.contains("MAX_PLAYERS")),
            "{}",
            err
        );

        let mut variables = BTreeMap::new();
        variables.insert("MAX_PLAYERS".to_string(), "64".to_string());
        apply_overrides(
            &mut blueprint(&yaml),
            &Overrides {
                variables,
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn external_id_validation() {
        assert!(validate_external_id("whmcs-123").is_ok());
        assert!(validate_external_id("svc:42.beta_1").is_ok());
        assert!(validate_external_id("").is_err());
        assert!(validate_external_id("has space").is_err());
        assert!(validate_external_id("../etc").is_err());
        assert!(validate_external_id(&"x".repeat(129)).is_err());
    }

    #[tokio::test]
    async fn store_round_trips_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvisionStore::new(dir.path());
        let record = ProvisionRecord {
            container_id: "c1".into(),
            external_id: "whmcs-1".into(),
            name: "One".into(),
            blueprint: "minecraft-paper".into(),
            game: "minecraft".into(),
            owner: Some("client-9".into()),
            resources: ProvisionResources {
                memory_mb: 2048,
                cpu_millicores: 1000,
                disk_mb: 10240,
            },
            ports: vec![AssignedPort {
                name: "game".into(),
                port: 20000,
                protocol: "tcp".into(),
                variable: Some("SERVER_PORT".into()),
            }],
            variables: BTreeMap::new(),
            created_at: 1,
            updated_at: 1,
        };
        store.save(record.clone()).await.unwrap();
        assert_eq!(store.get("c1").await.unwrap().external_id, "whmcs-1");
        assert_eq!(
            store.find_by_external_id("whmcs-1").await.unwrap().container_id,
            "c1"
        );
        assert!(store.find_by_external_id("other").await.is_none());

        // A fresh store reads it back from disk.
        let again = ProvisionStore::new(dir.path());
        assert_eq!(again.load().await, 1);
        assert_eq!(again.get("c1").await.unwrap().primary_port(), Some(20000));

        assert!(again.remove("c1").await.is_some());
        assert!(again.get("c1").await.is_none());
        assert!(!dir.path().join(RECORD_SUBDIR).join("c1.json").exists());
    }

    #[tokio::test]
    async fn store_skips_corrupt_records() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join(RECORD_SUBDIR);
        std::fs::create_dir_all(&records).unwrap();
        std::fs::write(records.join("bad.json"), b"{not json").unwrap();
        let store = ProvisionStore::new(dir.path());
        assert_eq!(store.load().await, 0);
    }

    #[test]
    fn dir_size_sums_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), vec![0u8; 100]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b"), vec![0u8; 50]).unwrap();
        assert_eq!(dir_size(dir.path()), 150);
        assert_eq!(dir_size(&dir.path().join("missing")), 0);
    }
}

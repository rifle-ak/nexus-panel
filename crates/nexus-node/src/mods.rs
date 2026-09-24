//! Installed mods: background install jobs, the per-server registry of what
//! is installed, and update checking.
//!
//! Installing a marketplace mod used to happen inline in the HTTP handler,
//! which was fine while every provider shipped a ~50 KB Oxide plugin. Steam
//! Workshop items are whole mod directories — Arma and DayZ mod sets run to
//! gigabytes — so an inline install means a request that hangs for many
//! minutes with no progress, at the mercy of every proxy timeout in between.
//!
//! Installs therefore run detached, exactly like the game-file update
//! executor: the caller starts a job and polls it. One install at a time per
//! server, since concurrent installs would race on the same files directory.
//!
//! Every successful install is recorded in the server's registry
//! (`DATA_DIR/.nexus/mods/<server>.json`): provider, id, version and where it
//! went. That record is what an update check compares against the
//! marketplace, what "Update" reinstalls from, and what the periodic checker
//! walks to flag or apply updates.

use chrono::{DateTime, Utc};
use nexus_marketplace::{InstalledMod, MarketplaceManager};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::error::{NodeError, Result};
use crate::notify::{Notification, Notifier, Severity};

pub use crate::update::UpdateStatus as InstallStatus;

/// A snapshot of a mod-install job, safe to serialize to the API.
#[derive(Debug, Clone, Serialize)]
pub struct ModInstallJob {
    /// Unique job id.
    pub id: String,
    /// Container this install targets.
    pub container_id: String,
    pub status: InstallStatus,
    /// Marketplace provider (e.g. `steam_workshop`).
    pub provider: String,
    /// Mod id within that provider.
    pub mod_id: String,
    /// Directory inside the server the mod is installed into.
    pub target_dir: String,
    /// Installed path relative to the server root, once finished.
    pub file_path: Option<String>,
    /// Installed size in bytes, once finished.
    pub file_size: Option<u64>,
    /// Content checksum, once finished.
    pub checksum: Option<String>,
    /// Signature keys copied into the server's `keys/` directory (DayZ/Arma).
    pub signature_keys: Vec<String>,
    /// Why the install failed, if it did.
    pub error: Option<String>,
    /// Unix seconds when the job started.
    pub started_at: u64,
    /// Unix seconds when the job finished.
    pub finished_at: Option<u64>,
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl ModInstallJob {
    fn new(
        id: String,
        container_id: String,
        provider: String,
        mod_id: String,
        target_dir: String,
    ) -> Self {
        Self {
            id,
            container_id,
            status: InstallStatus::Running,
            provider,
            mod_id,
            target_dir,
            file_path: None,
            file_size: None,
            checksum: None,
            signature_keys: Vec::new(),
            error: None,
            started_at: unix_secs(SystemTime::now()),
            finished_at: None,
        }
    }
}

/// In-memory registry of install jobs, one active job per container.
#[derive(Default)]
pub struct ModInstallJobStore {
    jobs: RwLock<HashMap<String, ModInstallJob>>,
}

impl ModInstallJobStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new running job for `container_id`, returning its snapshot.
    /// Fails if an install is already running for that container.
    pub async fn start(
        &self,
        container_id: &str,
        provider: &str,
        mod_id: &str,
        target_dir: &str,
    ) -> std::result::Result<ModInstallJob, String> {
        let mut jobs = self.jobs.write().await;
        if let Some(existing) = jobs.get(container_id) {
            if existing.status == InstallStatus::Running {
                return Err("a mod install is already running for this server".to_string());
            }
        }
        let job = ModInstallJob::new(
            format!("mod-{}", uuid::Uuid::new_v4()),
            container_id.to_string(),
            provider.to_string(),
            mod_id.to_string(),
            target_dir.to_string(),
        );
        jobs.insert(container_id.to_string(), job.clone());
        Ok(job)
    }

    /// Get the current job snapshot for a container, if any.
    pub async fn get(&self, container_id: &str) -> Option<ModInstallJob> {
        self.jobs.read().await.get(container_id).cloned()
    }

    /// Mark a job successfully finished.
    pub async fn succeed(
        &self,
        container_id: &str,
        file_path: String,
        file_size: u64,
        checksum: String,
        signature_keys: Vec<String>,
    ) {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(container_id) {
            job.status = InstallStatus::Succeeded;
            job.file_path = Some(file_path);
            job.file_size = Some(file_size);
            job.checksum = Some(checksum);
            job.signature_keys = signature_keys;
            job.finished_at = Some(unix_secs(SystemTime::now()));
        }
    }

    /// Mark a job failed.
    pub async fn fail(&self, container_id: &str, error: String) {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(container_id) {
            job.status = InstallStatus::Failed;
            job.error = Some(error);
            job.finished_at = Some(unix_secs(SystemTime::now()));
        }
    }
}

/// Shared handle used by the web layer.
pub type SharedModInstallJobStore = Arc<ModInstallJobStore>;

// ---------------------------------------------------------------------------
// Installed-mods registry
// ---------------------------------------------------------------------------

/// A mod the panel installed into a server, as recorded in its registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModRecord {
    /// Marketplace provider (e.g. `umod`, `steam_workshop`).
    pub provider: String,
    /// Mod id within that provider.
    pub mod_id: String,
    /// Display name, as the marketplace reported it at install time.
    pub name: String,
    /// The version installed. Workshop items carry their last-updated
    /// timestamp here, since they have no version strings.
    pub version: String,
    /// Where it landed, relative to the server root (`oxide/plugins/X.cs`,
    /// `@CF`).
    pub path: String,
    /// The directory it was installed into, relative to the server root.
    /// An update reinstalls into the same place.
    pub target_dir: String,
    pub file_size: u64,
    pub checksum: String,
    /// Signature keys copied into `keys/` (DayZ/Arma); removed on uninstall.
    #[serde(default)]
    pub signature_keys: Vec<String>,
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Apply new versions on the periodic check instead of only flagging them.
    #[serde(default)]
    pub auto_update: bool,
    /// A newer version the last check found, if any.
    #[serde(default)]
    pub available_version: Option<String>,
    /// Its changelog, when the provider publishes one.
    #[serde(default)]
    pub changelog: Option<String>,
    /// When this mod was last checked against the marketplace.
    #[serde(default)]
    pub checked_at: Option<DateTime<Utc>>,
}

impl ModRecord {
    fn is(&self, provider: &str, mod_id: &str) -> bool {
        self.provider == provider && self.mod_id == mod_id
    }

    /// The marketplace crate's view of this record.
    pub fn to_installed(&self) -> InstalledMod {
        InstalledMod {
            provider: self.provider.clone(),
            mod_id: self.mod_id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            path: PathBuf::from(&self.path),
            installed_at: self.installed_at,
            updated_at: self.updated_at,
            auto_update: self.auto_update,
        }
    }
}

/// What the node has installed into each server, kept as one JSON file per
/// server under `DATA_DIR/.nexus/mods/`.
pub struct ModRegistry {
    dir: PathBuf,
    /// Serialises read-modify-write cycles. Registries are tiny and writes
    /// rare, so one lock for all servers is fine.
    lock: Mutex<()>,
}

/// A server id is used as a file name; refuse anything that could leave the
/// registry directory.
fn safe_id(id: &str) -> Result<&str> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        || id.starts_with('.')
    {
        return Err(NodeError::InvalidInput(format!(
            "invalid server id {:?}",
            id
        )));
    }
    Ok(id)
}

impl ModRegistry {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(".nexus").join("mods"),
            lock: Mutex::new(()),
        }
    }

    fn file(&self, container_id: &str) -> Result<PathBuf> {
        Ok(self.dir.join(format!("{}.json", safe_id(container_id)?)))
    }

    async fn read(&self, container_id: &str) -> Result<Vec<ModRecord>> {
        let path = self.file(container_id)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                NodeError::Internal(format!(
                    "mod registry {} is unreadable: {}",
                    path.display(),
                    e
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    async fn write(&self, container_id: &str, records: &[ModRecord]) -> Result<()> {
        let path = self.file(container_id)?;
        if records.is_empty() {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            return Ok(());
        }
        tokio::fs::create_dir_all(&self.dir).await?;
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(records)
            .map_err(|e| NodeError::Internal(format!("serialize mod registry: {}", e)))?;
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    /// Everything recorded for a server, in install order.
    pub async fn list(&self, container_id: &str) -> Result<Vec<ModRecord>> {
        self.read(container_id).await
    }

    /// One record, if the mod is installed.
    pub async fn get(&self, container_id: &str, provider: &str, mod_id: &str) -> Result<ModRecord> {
        self.read(container_id)
            .await?
            .into_iter()
            .find(|r| r.is(provider, mod_id))
            .ok_or_else(|| {
                NodeError::InvalidInput(format!("{}/{} is not installed", provider, mod_id))
            })
    }

    /// Record an install (or reinstall). A mod already on record keeps its
    /// original install time and auto-update choice; any flagged update is
    /// cleared, since this is the new installed version.
    pub async fn record(&self, container_id: &str, mut record: ModRecord) -> Result<ModRecord> {
        let _guard = self.lock.lock().await;
        let mut records = self.read(container_id).await?;
        match records.iter_mut().find(|r| r.is(&record.provider, &record.mod_id)) {
            Some(existing) => {
                record.installed_at = existing.installed_at;
                record.auto_update = existing.auto_update;
                record.available_version = None;
                record.changelog = None;
                record.checked_at = existing.checked_at;
                *existing = record.clone();
            }
            None => records.push(record.clone()),
        }
        self.write(container_id, &records).await?;
        Ok(record)
    }

    pub async fn set_auto_update(
        &self,
        container_id: &str,
        provider: &str,
        mod_id: &str,
        auto_update: bool,
    ) -> Result<ModRecord> {
        let _guard = self.lock.lock().await;
        let mut records = self.read(container_id).await?;
        let record = records.iter_mut().find(|r| r.is(provider, mod_id)).ok_or_else(|| {
            NodeError::InvalidInput(format!("{}/{} is not installed", provider, mod_id))
        })?;
        record.auto_update = auto_update;
        let record = record.clone();
        self.write(container_id, &records).await?;
        Ok(record)
    }

    /// Drop a mod from the registry, returning what was recorded.
    pub async fn remove(
        &self,
        container_id: &str,
        provider: &str,
        mod_id: &str,
    ) -> Result<ModRecord> {
        let _guard = self.lock.lock().await;
        let mut records = self.read(container_id).await?;
        let pos = records.iter().position(|r| r.is(provider, mod_id)).ok_or_else(|| {
            NodeError::InvalidInput(format!("{}/{} is not installed", provider, mod_id))
        })?;
        let record = records.remove(pos);
        self.write(container_id, &records).await?;
        Ok(record)
    }

    /// Drop a deleted server's registry.
    pub async fn forget(&self, container_id: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        self.write(container_id, &[]).await
    }

    /// Servers that have a registry.
    pub async fn servers(&self) -> Vec<String> {
        let mut ids = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(_) => return ids,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                ids.push(id.to_string());
            }
        }
        ids.sort();
        ids
    }

    /// Ask the marketplace whether newer versions exist for everything
    /// installed in a server, and note the answer on each record.
    ///
    /// The network round trip happens outside the registry lock so a slow
    /// provider does not stall installs elsewhere.
    pub async fn check(
        &self,
        container_id: &str,
        marketplace: &MarketplaceManager,
    ) -> Result<Vec<ModRecord>> {
        let current = self.read(container_id).await?;
        if current.is_empty() {
            return Ok(current);
        }
        let installed: Vec<InstalledMod> = current.iter().map(ModRecord::to_installed).collect();
        let updates = marketplace
            .check_updates(&installed)
            .await
            .map_err(|e| NodeError::Internal(format!("update check failed: {}", e)))?;
        let now = Utc::now();

        let _guard = self.lock.lock().await;
        let mut records = self.read(container_id).await?;
        for record in &mut records {
            // Only records the check actually covered; an install that landed
            // meanwhile has not been checked.
            if !current.iter().any(|c| c.is(&record.provider, &record.mod_id)) {
                continue;
            }
            record.checked_at = Some(now);
            match updates.iter().find(|u| record.is(&u.installed.provider, &u.installed.mod_id)) {
                Some(update) if update.latest_version != record.version => {
                    record.available_version = Some(update.latest_version.clone());
                    record.changelog = update.changelog.clone();
                }
                _ => {
                    record.available_version = None;
                    record.changelog = None;
                }
            }
        }
        self.write(container_id, &records).await?;
        Ok(records)
    }
}

// ---------------------------------------------------------------------------
// Running an install
// ---------------------------------------------------------------------------

/// Everything an install needs to know; the job must already be registered.
#[derive(Debug, Clone)]
pub struct InstallSpec {
    pub container_id: String,
    pub provider: String,
    pub mod_id: String,
    /// A specific version, or the latest when `None`.
    pub version: Option<String>,
    /// The install directory relative to the server root, as recorded.
    pub target_dir: String,
    /// The same directory, resolved on disk.
    pub target: PathBuf,
    pub server_dir: PathBuf,
}

/// Run a mod install to completion, record the outcome on `jobs`, and add the
/// mod to the server's registry.
///
/// Split out from the handler so the spawned task stays readable — and so the
/// path-relativizing logic (the API reports paths relative to the server root)
/// lives next to the job bookkeeping rather than in the route.
pub async fn run_install(
    jobs: SharedModInstallJobStore,
    marketplace: Arc<MarketplaceManager>,
    registry: Arc<ModRegistry>,
    spec: InstallSpec,
) -> std::result::Result<ModRecord, String> {
    let InstallSpec {
        container_id,
        provider,
        mod_id,
        version,
        target_dir,
        target,
        server_dir,
    } = spec;
    let result = marketplace.download_mod(&provider, &mod_id, version.as_deref(), &target).await;
    let result = match result {
        Ok(result) => result,
        Err(e) => {
            warn!(
                "Failed to install {}/{} for server {}: {}",
                provider, mod_id, container_id, e
            );
            jobs.fail(&container_id, e.to_string()).await;
            return Err(e.to_string());
        }
    };

    let rel = relative_to(&result.file_path, &server_dir);
    info!(
        "Installed {}/{} into {} for server {}",
        provider, mod_id, rel, container_id
    );

    // The download resolved "latest" from the same (cached) details, so this
    // names exactly what landed on disk.
    let (name, installed_version) = match marketplace.get_mod(&provider, &mod_id).await {
        Ok(details) => {
            let v = version
                .clone()
                .or_else(|| details.versions.first().map(|v| v.version.clone()))
                .unwrap_or(details.info.latest_version);
            (details.info.name, v)
        }
        Err(_) => (mod_id.clone(), version.clone().unwrap_or_default()),
    };
    let now = Utc::now();
    let record = ModRecord {
        provider: provider.clone(),
        mod_id: mod_id.clone(),
        name,
        version: installed_version,
        path: rel.clone(),
        target_dir,
        file_size: result.file_size,
        checksum: result.checksum.clone(),
        signature_keys: result.signature_keys.clone(),
        installed_at: now,
        updated_at: now,
        auto_update: false,
        available_version: None,
        changelog: None,
        checked_at: None,
    };
    let recorded = match registry.record(&container_id, record.clone()).await {
        Ok(recorded) => recorded,
        Err(e) => {
            // The files are in place; a missing record only costs update
            // checks for this mod, so report success and say so in the log.
            warn!(
                "Installed {}/{} for server {} but could not record it: {}",
                provider, mod_id, container_id, e
            );
            record
        }
    };
    jobs.succeed(
        &container_id,
        rel,
        result.file_size,
        result.checksum,
        result.signature_keys,
    )
    .await;
    Ok(recorded)
}

/// Render an installed path relative to the server root, for display.
fn relative_to(path: &Path, server_dir: &Path) -> String {
    path.strip_prefix(server_dir).unwrap_or(path).to_string_lossy().to_string()
}

// ---------------------------------------------------------------------------
// Periodic update checks
// ---------------------------------------------------------------------------

/// How often the node checks installed mods against their marketplaces.
/// `NEXUS_MOD_UPDATE_CHECK_HOURS`, default 6; `0` disables the check.
pub fn check_interval_from_env() -> Option<std::time::Duration> {
    let hours = std::env::var("NEXUS_MOD_UPDATE_CHECK_HOURS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(6);
    (hours > 0).then(|| std::time::Duration::from_secs(hours * 3600))
}

/// What the periodic checker needs.
pub struct UpdateChecker {
    pub registry: Arc<ModRegistry>,
    pub marketplace: Arc<MarketplaceManager>,
    pub jobs: SharedModInstallJobStore,
    pub manager: Arc<crate::container::ContainerManager>,
    pub notifier: Arc<Notifier>,
    pub data_dir: PathBuf,
}

/// The outcome of one pass over one server, for logging and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckOutcome {
    /// Mods a newer version was found for since the previous check.
    pub newly_available: Vec<String>,
    /// Mods updated because they opted in.
    pub updated: Vec<String>,
    /// Mods whose automatic update failed.
    pub failed: Vec<String>,
}

impl UpdateChecker {
    /// Check every server on a schedule, forever.
    pub async fn run(self, every: std::time::Duration) {
        // Let the node settle (and the marketplaces recover from the burst of
        // a restart) before the first pass.
        let start = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut interval = tokio::time::interval_at(start, every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.check_all().await;
        }
    }

    /// One pass over every server with a registry.
    pub async fn check_all(&self) {
        for id in self.registry.servers().await {
            // A registry left behind by a deleted server is not worth a
            // network round trip.
            if self.manager.get_state(&id).await.is_err() {
                continue;
            }
            match self.check_server(&id).await {
                Ok(outcome) => {
                    if outcome != CheckOutcome::default() {
                        info!(
                            "Mod check for server {}: {} new update(s), {} applied, {} failed",
                            id,
                            outcome.newly_available.len(),
                            outcome.updated.len(),
                            outcome.failed.len()
                        );
                    }
                }
                Err(e) => warn!("Mod update check for server {} failed: {}", id, e),
            }
        }
    }

    /// Check one server: flag new versions, apply them where the mod opted
    /// in, and tell the operator either way.
    pub async fn check_server(&self, container_id: &str) -> Result<CheckOutcome> {
        let name = self
            .manager
            .get_state(container_id)
            .await
            .map(|s| s.name)
            .unwrap_or_else(|_| container_id.to_string());
        let before = self.registry.list(container_id).await?;
        let after = self.registry.check(container_id, &self.marketplace).await?;
        let mut outcome = CheckOutcome::default();

        for record in after.iter().filter(|r| r.available_version.is_some()) {
            let seen = before
                .iter()
                .find(|b| b.is(&record.provider, &record.mod_id))
                .and_then(|b| b.available_version.clone());
            if seen != record.available_version {
                outcome.newly_available.push(record.name.clone());
            }
        }

        // Tell the operator about versions they have not seen before, once,
        // for mods they will have to update by hand.
        let manual: Vec<String> = after
            .iter()
            .filter(|r| r.available_version.is_some() && !r.auto_update)
            .filter(|r| outcome.newly_available.contains(&r.name))
            .map(|r| {
                format!(
                    "{} {} → {}",
                    r.name,
                    r.version,
                    r.available_version.as_deref().unwrap_or("?")
                )
            })
            .collect();
        if !manual.is_empty() {
            self.notifier.notify(
                Notification::new(
                    "mod.update_available",
                    Severity::Info,
                    format!("{} mod update(s) available for {}", manual.len(), name),
                    manual.join("\n"),
                )
                .for_server(container_id, &name),
            );
        }

        for record in after.iter().filter(|r| r.auto_update && r.available_version.is_some()) {
            let latest = record.available_version.clone().unwrap_or_default();
            match self.apply(container_id, record, &latest).await {
                Ok(()) => {
                    outcome.updated.push(record.name.clone());
                    self.notifier.notify(
                        Notification::new(
                            "mod.updated",
                            Severity::Info,
                            format!("{} updated on {}", record.name, name),
                            format!("{} {} → {}", record.name, record.version, latest),
                        )
                        .for_server(container_id, &name),
                    );
                }
                Err(e) => {
                    outcome.failed.push(record.name.clone());
                    self.notifier.notify(
                        Notification::new(
                            "mod.update_failed",
                            Severity::Warning,
                            format!("{} could not be updated on {}", record.name, name),
                            format!("{} {} → {}: {}", record.name, record.version, latest, e),
                        )
                        .for_server(container_id, &name),
                    );
                }
            }
        }
        Ok(outcome)
    }

    async fn apply(&self, container_id: &str, record: &ModRecord, version: &str) -> Result<()> {
        let fm = crate::files::FileManager::new(container_id, &self.data_dir)
            .with_owner(self.manager.game_user());
        let target = fm.resolve_path(&record.target_dir)?;
        self.jobs
            .start(
                container_id,
                &record.provider,
                &record.mod_id,
                &record.target_dir,
            )
            .await
            .map_err(NodeError::Internal)?;
        let spec = InstallSpec {
            container_id: container_id.to_string(),
            provider: record.provider.clone(),
            mod_id: record.mod_id.clone(),
            version: Some(version.to_string()),
            target_dir: record.target_dir.clone(),
            target,
            server_dir: fm.server_dir().to_path_buf(),
        };
        run_install(
            self.jobs.clone(),
            self.marketplace.clone(),
            self.registry.clone(),
            spec,
        )
        .await
        .map(|_| ())
        .map_err(NodeError::Internal)
    }
}

/// A marketplace that serves whatever versions a test tells it to.
#[cfg(test)]
pub(crate) mod testing {
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use nexus_marketplace::adapters::MarketplaceAdapter;
    use nexus_marketplace::{
        Category, DownloadResult, MarketplaceError, ModDetails, ModInfo, SearchQuery, VersionInfo,
    };
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// Versions per mod id, newest first.
    #[derive(Clone, Default)]
    pub struct FakeMarket {
        pub versions: Arc<Mutex<std::collections::HashMap<String, Vec<String>>>>,
    }

    impl FakeMarket {
        pub fn publish(&self, mod_id: &str, versions: &[&str]) {
            self.versions.lock().unwrap().insert(
                mod_id.to_string(),
                versions.iter().map(|v| v.to_string()).collect(),
            );
        }

        fn details(&self, mod_id: &str) -> nexus_marketplace::Result<ModDetails> {
            let versions = self
                .versions
                .lock()
                .unwrap()
                .get(mod_id)
                .cloned()
                .ok_or_else(|| MarketplaceError::ModNotFound(mod_id.to_string()))?;
            let latest = versions.first().cloned().unwrap_or_default();
            let info = ModInfo {
                id: mod_id.to_string(),
                name: format!("Mod {}", mod_id),
                description: "test".into(),
                author: "tester".into(),
                provider: "fake".into(),
                game: "rust".into(),
                category: None,
                downloads: 1,
                rating: None,
                latest_version: latest,
                updated_at: Utc.timestamp_opt(0, 0).unwrap(),
                url: String::new(),
                icon_url: None,
            };
            Ok(ModDetails {
                info,
                full_description: None,
                versions: versions
                    .into_iter()
                    .map(|v| VersionInfo {
                        version: v.clone(),
                        download_url: String::new(),
                        file_size: 0,
                        checksum: None,
                        release_date: Utc.timestamp_opt(0, 0).unwrap(),
                        changelog: Some(format!("changes in {}", v)),
                        min_game_version: None,
                        max_game_version: None,
                        downloads: 0,
                    })
                    .collect(),
                dependencies: vec![],
                screenshots: vec![],
                license: None,
                source_url: None,
                issues_url: None,
                community_url: None,
            })
        }
    }

    #[async_trait]
    impl MarketplaceAdapter for FakeMarket {
        fn provider_name(&self) -> &str {
            "fake"
        }
        fn supported_games(&self) -> Vec<String> {
            vec!["rust".into()]
        }
        async fn search(&self, _q: &SearchQuery) -> nexus_marketplace::Result<Vec<ModInfo>> {
            Ok(vec![])
        }
        async fn get_mod_details(&self, mod_id: &str) -> nexus_marketplace::Result<ModDetails> {
            self.details(mod_id)
        }
        async fn get_categories(&self, _game: &str) -> nexus_marketplace::Result<Vec<Category>> {
            Ok(vec![])
        }
        async fn download(
            &self,
            mod_id: &str,
            version: &str,
            target_dir: &Path,
        ) -> nexus_marketplace::Result<DownloadResult> {
            if version == "broken" {
                return Err(MarketplaceError::DownloadFailed {
                    reason: "simulated download failure".into(),
                });
            }
            tokio::fs::create_dir_all(target_dir).await?;
            let file = target_dir.join(format!("{}.cs", mod_id));
            tokio::fs::write(&file, version.as_bytes()).await?;
            Ok(DownloadResult {
                file_path: file,
                file_size: version.len() as u64,
                checksum: format!("sum-{}", version),
                download_time_ms: 1,
                signature_keys: vec![],
            })
        }
    }

    /// A manager with only the fake provider registered, and the market
    /// handle to publish versions through.
    pub fn market() -> (Arc<nexus_marketplace::MarketplaceManager>, FakeMarket) {
        let fake = FakeMarket::default();
        let mut mgr = nexus_marketplace::MarketplaceManager::new();
        mgr.register_adapter(fake.clone());
        (Arc::new(mgr), fake)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::market;
    use super::*;

    #[test]
    fn installed_paths_are_reported_relative_to_the_server_root() {
        assert_eq!(
            relative_to(Path::new("/data/srv1/@CF"), Path::new("/data/srv1")),
            "@CF"
        );
        assert_eq!(
            relative_to(
                Path::new("/data/srv1/oxide/plugins/X.cs"),
                Path::new("/data/srv1")
            ),
            "oxide/plugins/X.cs"
        );
        // A path outside the server dir is reported as-is rather than mangled.
        assert_eq!(
            relative_to(Path::new("/elsewhere/x"), Path::new("/data/srv1")),
            "/elsewhere/x"
        );
    }

    #[tokio::test]
    async fn store_rejects_a_second_install_until_the_first_finishes() {
        let store = ModInstallJobStore::new();
        let job = store.start("c1", "steam_workshop", "1559212036", ".").await.unwrap();
        assert_eq!(job.status, InstallStatus::Running);
        assert!(job.id.starts_with("mod-"));

        // Concurrent installs would race on the same files directory.
        assert!(store.start("c1", "umod", "other", ".").await.is_err());

        store
            .succeed(
                "c1",
                "@CF".to_string(),
                527656,
                "abc".to_string(),
                vec!["cf.bikey".to_string()],
            )
            .await;

        let done = store.get("c1").await.unwrap();
        assert_eq!(done.status, InstallStatus::Succeeded);
        assert_eq!(done.file_path.as_deref(), Some("@CF"));
        assert_eq!(done.signature_keys, vec!["cf.bikey".to_string()]);
        assert!(done.finished_at.is_some());

        // Once finished, the next install may start.
        assert!(store.start("c1", "umod", "other", "oxide/plugins").await.is_ok());
    }

    #[tokio::test]
    async fn failures_are_recorded_with_their_reason() {
        let store = ModInstallJobStore::new();
        store.start("c2", "steam_workshop", "1", ".").await.unwrap();
        store.fail("c2", "steamcmd not found".to_string()).await;

        let job = store.get("c2").await.unwrap();
        assert_eq!(job.status, InstallStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("steamcmd not found"));
        assert!(job.file_path.is_none());
    }

    #[tokio::test]
    async fn unknown_server_has_no_job() {
        let store = ModInstallJobStore::new();
        assert!(store.get("nope").await.is_none());
    }

    fn record(provider: &str, mod_id: &str, version: &str) -> ModRecord {
        let now = Utc::now();
        ModRecord {
            provider: provider.into(),
            mod_id: mod_id.into(),
            name: format!("Mod {}", mod_id),
            version: version.into(),
            path: format!("oxide/plugins/{}.cs", mod_id),
            target_dir: "oxide/plugins".into(),
            file_size: 1,
            checksum: "x".into(),
            signature_keys: vec![],
            installed_at: now,
            updated_at: now,
            auto_update: false,
            available_version: None,
            changelog: None,
            checked_at: None,
        }
    }

    #[tokio::test]
    async fn registry_persists_per_server_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ModRegistry::new(dir.path());
        assert!(reg.list("s1").await.unwrap().is_empty());

        reg.record("s1", record("fake", "a", "1.0")).await.unwrap();
        reg.record("s1", record("fake", "b", "2.0")).await.unwrap();
        reg.record("s2", record("fake", "a", "1.0")).await.unwrap();
        reg.set_auto_update("s1", "fake", "b", true).await.unwrap();

        // A fresh instance reads the same files.
        let reg = ModRegistry::new(dir.path());
        let s1 = reg.list("s1").await.unwrap();
        assert_eq!(s1.len(), 2);
        assert!(s1[1].auto_update);
        assert_eq!(reg.list("s2").await.unwrap().len(), 1);
        assert_eq!(
            reg.servers().await,
            vec!["s1".to_string(), "s2".to_string()]
        );

        // Reinstalling keeps the install date and the auto-update choice, and
        // clears a flagged update since this *is* the update.
        let first_installed = s1[1].installed_at;
        let mut newer = record("fake", "b", "2.1");
        newer.available_version = Some("junk".into());
        let stored = reg.record("s1", newer).await.unwrap();
        assert_eq!(stored.version, "2.1");
        assert_eq!(stored.installed_at, first_installed);
        assert!(stored.auto_update);
        assert!(stored.available_version.is_none());
        assert_eq!(reg.list("s1").await.unwrap().len(), 2);

        // Removal and forgetting.
        assert!(reg.remove("s1", "fake", "nope").await.is_err());
        reg.remove("s1", "fake", "a").await.unwrap();
        assert_eq!(reg.list("s1").await.unwrap().len(), 1);
        reg.forget("s2").await.unwrap();
        assert_eq!(reg.servers().await, vec!["s1".to_string()]);
        reg.forget("never-existed").await.unwrap();
    }

    #[tokio::test]
    async fn registry_refuses_ids_that_could_escape_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ModRegistry::new(dir.path());
        for bad in ["../x", "a/b", "", ".hidden", "x\\y"] {
            assert!(reg.list(bad).await.is_err(), "{:?}", bad);
            assert!(reg.record(bad, record("fake", "a", "1")).await.is_err());
        }
    }

    #[tokio::test]
    async fn check_flags_newer_versions_and_clears_them_once_installed() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ModRegistry::new(dir.path());
        let (market, fake) = market();
        fake.publish("a", &["1.0"]);
        fake.publish("b", &["2.0"]);
        reg.record("s1", record("fake", "a", "1.0")).await.unwrap();
        reg.record("s1", record("fake", "b", "2.0")).await.unwrap();

        // Nothing new yet.
        let checked = reg.check("s1", &market).await.unwrap();
        assert!(checked.iter().all(|r| r.available_version.is_none()));
        assert!(checked.iter().all(|r| r.checked_at.is_some()));

        // A release appears.
        fake.publish("b", &["2.1", "2.0"]);
        let checked = reg.check("s1", &market).await.unwrap();
        assert!(checked[0].available_version.is_none());
        assert_eq!(checked[1].available_version.as_deref(), Some("2.1"));
        assert_eq!(checked[1].changelog.as_deref(), Some("changes in 2.1"));

        // Installing it clears the flag.
        reg.record("s1", record("fake", "b", "2.1")).await.unwrap();
        let checked = reg.check("s1", &market).await.unwrap();
        assert!(checked[1].available_version.is_none());

        // A mod the provider no longer knows is left as it was, not flagged.
        reg.record("s1", record("fake", "gone", "1")).await.unwrap();
        let checked = reg.check("s1", &market).await.unwrap();
        assert!(checked[2].available_version.is_none());

        // An unknown provider is skipped rather than failing the whole check.
        reg.record("s1", record("nowhere", "z", "1")).await.unwrap();
        assert!(reg.check("s1", &market).await.is_ok());
    }

    #[tokio::test]
    async fn install_records_what_landed_and_updates_reinstall_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();
        let server_dir = data_dir.join("s1");
        let reg = Arc::new(ModRegistry::new(data_dir));
        let jobs = Arc::new(ModInstallJobStore::new());
        let (market, fake) = market();
        fake.publish("a", &["1.0"]);

        let spec = InstallSpec {
            container_id: "s1".into(),
            provider: "fake".into(),
            mod_id: "a".into(),
            version: None,
            target_dir: "oxide/plugins".into(),
            target: server_dir.join("oxide/plugins"),
            server_dir: server_dir.clone(),
        };
        jobs.start("s1", "fake", "a", "oxide/plugins").await.unwrap();
        let rec = run_install(jobs.clone(), market.clone(), reg.clone(), spec.clone())
            .await
            .unwrap();
        assert_eq!(rec.version, "1.0");
        assert_eq!(rec.name, "Mod a");
        assert_eq!(rec.path, "oxide/plugins/a.cs");
        assert_eq!(rec.target_dir, "oxide/plugins");
        assert_eq!(
            std::fs::read_to_string(server_dir.join("oxide/plugins/a.cs")).unwrap(),
            "1.0"
        );
        assert_eq!(
            jobs.get("s1").await.unwrap().status,
            InstallStatus::Succeeded
        );
        assert_eq!(reg.list("s1").await.unwrap(), vec![rec]);

        // A newer version shows up; installing it explicitly updates the
        // record in place (same install date, new version).
        fake.publish("a", &["1.1", "1.0"]);
        let checked = reg.check("s1", &market).await.unwrap();
        assert_eq!(checked[0].available_version.as_deref(), Some("1.1"));

        jobs.start("s1", "fake", "a", "oxide/plugins").await.unwrap();
        let updated = run_install(
            jobs.clone(),
            market.clone(),
            reg.clone(),
            InstallSpec {
                version: Some("1.1".into()),
                ..spec.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.version, "1.1");
        assert!(updated.available_version.is_none());
        assert_eq!(
            std::fs::read_to_string(server_dir.join("oxide/plugins/a.cs")).unwrap(),
            "1.1"
        );
        assert_eq!(reg.list("s1").await.unwrap().len(), 1);

        // A failed download fails the job and leaves the record alone.
        // (A check refreshes the marketplace's metadata cache, so the
        // download sees the version the check found.)
        fake.publish("a", &["broken", "1.1", "1.0"]);
        reg.check("s1", &market).await.unwrap();
        jobs.start("s1", "fake", "a", "oxide/plugins").await.unwrap();
        let err = run_install(
            jobs.clone(),
            market.clone(),
            reg.clone(),
            InstallSpec {
                version: Some("broken".into()),
                ..spec
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("simulated"), "{}", err);
        assert_eq!(jobs.get("s1").await.unwrap().status, InstallStatus::Failed);
        assert_eq!(reg.list("s1").await.unwrap()[0].version, "1.1");
    }

    #[tokio::test]
    async fn interval_env_is_hours_and_zero_disables() {
        // Not set: the default.
        std::env::remove_var("NEXUS_MOD_UPDATE_CHECK_HOURS");
        assert_eq!(
            check_interval_from_env(),
            Some(std::time::Duration::from_secs(6 * 3600))
        );
        std::env::set_var("NEXUS_MOD_UPDATE_CHECK_HOURS", "0");
        assert_eq!(check_interval_from_env(), None);
        std::env::set_var("NEXUS_MOD_UPDATE_CHECK_HOURS", "1");
        assert_eq!(
            check_interval_from_env(),
            Some(std::time::Duration::from_secs(3600))
        );
        std::env::remove_var("NEXUS_MOD_UPDATE_CHECK_HOURS");
    }
}

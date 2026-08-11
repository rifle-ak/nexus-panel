//! Background mod-install jobs.
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

use nexus_marketplace::MarketplaceManager;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use tracing::{info, warn};

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
    ) -> Result<ModInstallJob, String> {
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

/// Run a mod install to completion and record the outcome on `jobs`.
///
/// Split out from the handler so the spawned task stays readable — and so the
/// path-relativizing logic (the API reports paths relative to the server root)
/// lives next to the job bookkeeping rather than in the route.
#[allow(clippy::too_many_arguments)]
pub async fn run_install(
    jobs: SharedModInstallJobStore,
    marketplace: Arc<MarketplaceManager>,
    container_id: String,
    provider: String,
    mod_id: String,
    version: Option<String>,
    target: std::path::PathBuf,
    server_dir: std::path::PathBuf,
) {
    match marketplace.download_mod(&provider, &mod_id, version.as_deref(), &target).await {
        Ok(result) => {
            let rel = relative_to(&result.file_path, &server_dir);
            info!(
                "Installed {}/{} into {} for server {}",
                provider, mod_id, rel, container_id
            );
            jobs.succeed(
                &container_id,
                rel,
                result.file_size,
                result.checksum,
                result.signature_keys,
            )
            .await;
        }
        Err(e) => {
            warn!(
                "Failed to install {}/{} for server {}: {}",
                provider, mod_id, container_id, e
            );
            jobs.fail(&container_id, e.to_string()).await;
        }
    }
}

/// Render an installed path relative to the server root, for display.
fn relative_to(path: &Path, server_dir: &Path) -> String {
    path.strip_prefix(server_dir).unwrap_or(path).to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
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
}

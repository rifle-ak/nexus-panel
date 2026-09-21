//! Backups of a server's files.
//!
//! A backup is a gzip-compressed tar of the server directory (or the paths
//! the blueprint names), with a SHA-256 checksum, kept under
//! `DATA_DIR/backups/<server>/<id>.tar.gz`. Next to each archive sits
//! `<id>.json`, the record the panel shows, so the list survives a restart
//! and an archive copied in from elsewhere is picked up on the next start.
//!
//! Restoring replaces the server directory atomically: the archive is
//! verified and unpacked beside the directory, then the two are swapped, so
//! a failure part-way leaves the server exactly as it was. The files come
//! out owned by the game user.

use crate::error::{NodeError, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tar::{Archive, Builder};
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Backup status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Backup information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInfo {
    pub id: String,
    pub container_id: String,
    pub name: String,
    pub size: u64,
    pub created_at: i64,
    pub checksum: String,
    pub status: BackupStatus,
    pub error: Option<String>,
    /// The paths this backup was limited to; empty means everything.
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Backup manager for containers
pub struct BackupManager {
    /// Base data directory
    data_dir: PathBuf,

    /// Backup storage directory
    backup_dir: PathBuf,

    /// The registry, mirrored on disk as one JSON file per backup.
    backups: Arc<RwLock<HashMap<String, HashMap<String, BackupInfo>>>>,

    /// Who restored files belong to.
    owner: Option<(u32, u32)>,
}

impl BackupManager {
    /// Create a new backup manager
    pub fn new(data_dir: &Path) -> Self {
        let backup_dir = data_dir.join("backups");
        Self {
            data_dir: data_dir.to_path_buf(),
            backup_dir,
            backups: Arc::new(RwLock::new(HashMap::new())),
            owner: None,
        }
    }

    /// Give restored files to `uid:gid` (the game user).
    pub fn with_owner(mut self, owner: (u32, u32)) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Create the storage directory and load the records already there.
    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.backup_dir).await?;
        let loaded = self.load_registry().await;
        if loaded > 0 {
            info!("Loaded {} backup record(s) from disk", loaded);
        }
        Ok(())
    }

    /// Read every `<id>.json` under the backup directory. An archive with
    /// no record (copied in by hand, or from an older build) gets one, so
    /// it can be restored; a record whose archive is gone is dropped.
    async fn load_registry(&self) -> usize {
        let Ok(containers) = std::fs::read_dir(&self.backup_dir) else {
            return 0;
        };
        let mut loaded = 0;
        for container in containers.flatten() {
            if !container.path().is_dir() {
                continue;
            }
            let container_id = container.file_name().to_string_lossy().into_owned();
            let Ok(entries) = std::fs::read_dir(container.path()) else {
                continue;
            };
            let mut records: HashMap<String, BackupInfo> = HashMap::new();
            let mut archives: Vec<String> = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(id) = name.strip_suffix(".json") {
                    match std::fs::read(entry.path())
                        .ok()
                        .and_then(|b| serde_json::from_slice::<BackupInfo>(&b).ok())
                    {
                        Some(info) if info.id == id => {
                            records.insert(id.to_string(), info);
                        }
                        _ => warn!("Skipping unreadable backup record {:?}", entry.path()),
                    }
                } else if let Some(id) = name.strip_suffix(".tar.gz") {
                    archives.push(id.to_string());
                }
            }
            // Records without archives: an interrupted backup, or a deleted
            // file. Archives without records: adopt them.
            records.retain(|id, info| {
                let present = archives.iter().any(|a| a == id);
                if !present && info.status == BackupStatus::Completed {
                    warn!(
                        "Backup {} of {} has no archive on disk; dropping its record",
                        id, container_id
                    );
                    let _ = std::fs::remove_file(self.record_path(&container_id, id));
                }
                present || info.status != BackupStatus::Completed
            });
            for id in archives {
                if records.contains_key(&id) {
                    if records[&id].status == BackupStatus::InProgress {
                        // The node died mid-backup; the archive is not trustworthy.
                        warn!(
                            "Backup {} of {} was interrupted; removing it",
                            id, container_id
                        );
                        let _ = std::fs::remove_file(self.archive_path(&container_id, &id));
                        let _ = std::fs::remove_file(self.record_path(&container_id, &id));
                        records.remove(&id);
                    }
                    continue;
                }
                let path = self.archive_path(&container_id, &id);
                let meta = std::fs::metadata(&path).ok();
                let info = BackupInfo {
                    id: id.clone(),
                    container_id: container_id.clone(),
                    name: "recovered".to_string(),
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    created_at: meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or_else(|| chrono::Utc::now().timestamp()),
                    // No checksum on record: restore skips verification.
                    checksum: String::new(),
                    status: BackupStatus::Completed,
                    error: None,
                    include: Vec::new(),
                    exclude: Vec::new(),
                };
                let _ = self.write_record(&info);
                records.insert(id, info);
            }
            loaded += records.len();
            if !records.is_empty() {
                self.backups.write().await.insert(container_id, records);
            }
        }
        loaded
    }

    fn archive_path(&self, container_id: &str, backup_id: &str) -> PathBuf {
        self.backup_dir.join(container_id).join(format!("{}.tar.gz", backup_id))
    }

    fn record_path(&self, container_id: &str, backup_id: &str) -> PathBuf {
        self.backup_dir.join(container_id).join(format!("{}.json", backup_id))
    }

    fn write_record(&self, info: &BackupInfo) -> Result<()> {
        let path = self.record_path(&info.container_id, &info.id);
        let bytes = serde_json::to_vec_pretty(info)
            .map_err(|e| NodeError::Internal(format!("serialize backup record: {}", e)))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Record a backup in memory and on disk.
    async fn register(&self, info: &BackupInfo) {
        self.backups
            .write()
            .await
            .entry(info.container_id.clone())
            .or_default()
            .insert(info.id.clone(), info.clone());
        if let Err(e) = self.write_record(info) {
            warn!("Could not write record for backup {}: {}", info.id, e);
        }
    }

    /// Create a backup of a container
    pub async fn create_backup(
        &self,
        container_id: &str,
        name: &str,
        include_paths: &[String],
        exclude_paths: &[String],
    ) -> Result<BackupInfo> {
        let backup_id = Uuid::new_v4().to_string();
        let container_dir = self.data_dir.join(container_id);
        let container_backup_dir = self.backup_dir.join(container_id);

        // Ensure container exists
        if !container_dir.exists() {
            return Err(NodeError::ContainerNotFound(container_id.to_string()));
        }

        // Create backup directory for this container
        fs::create_dir_all(&container_backup_dir).await?;

        let backup_path = self.archive_path(container_id, &backup_id);

        let mut backup_info = BackupInfo {
            id: backup_id.clone(),
            container_id: container_id.to_string(),
            name: name.to_string(),
            size: 0,
            created_at: chrono::Utc::now().timestamp(),
            checksum: String::new(),
            status: BackupStatus::InProgress,
            error: None,
            include: include_paths.iter().map(|p| normalize(p)).collect(),
            exclude: exclude_paths.iter().map(|p| normalize(p)).collect(),
        };
        self.register(&backup_info).await;

        match self
            .create_backup_archive(&container_dir, &backup_path, include_paths, exclude_paths)
            .await
        {
            Ok((size, checksum)) => {
                backup_info.size = size;
                backup_info.checksum = checksum;
                backup_info.status = BackupStatus::Completed;
                info!(
                    "Created backup {} for container {} ({} bytes)",
                    backup_id, container_id, size
                );
            }
            Err(e) => {
                backup_info.status = BackupStatus::Failed;
                backup_info.error = Some(e.to_string());
                warn!(
                    "Failed to create backup {} for container {}: {}",
                    backup_id, container_id, e
                );
            }
        }

        if backup_info.status == BackupStatus::Failed {
            // A failed backup leaves nothing behind but its error.
            let _ = fs::remove_file(&backup_path).await;
            self.register(&backup_info).await;
            return Err(NodeError::Internal(
                backup_info.error.unwrap_or_else(|| "Backup failed".to_string()),
            ));
        }

        self.register(&backup_info).await;
        Ok(backup_info)
    }

    /// Delete the oldest completed backups beyond `keep`. Returns the ids
    /// removed.
    pub async fn enforce_retention(&self, container_id: &str, keep: u32) -> Result<Vec<String>> {
        if keep == 0 {
            return Ok(Vec::new());
        }
        let mut completed: Vec<BackupInfo> = self
            .list_backups(container_id)
            .await?
            .into_iter()
            .filter(|b| b.status == BackupStatus::Completed)
            .collect();
        // Newest first, as list_backups returns them.
        completed.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        let mut removed = Vec::new();
        for old in completed.into_iter().skip(keep as usize) {
            self.delete_backup(container_id, &old.id).await?;
            removed.push(old.id);
        }
        if !removed.is_empty() {
            info!(
                "Retention ({}) removed {} backup(s) of {}",
                keep,
                removed.len(),
                container_id
            );
        }
        Ok(removed)
    }

    /// Create the backup archive
    async fn create_backup_archive(
        &self,
        source_dir: &Path,
        output_path: &Path,
        include_paths: &[String],
        exclude_paths: &[String],
    ) -> Result<(u64, String)> {
        let source = source_dir.to_path_buf();
        let output = output_path.to_path_buf();
        let includes: Vec<String> = include_paths.iter().map(|p| normalize(p)).collect();
        let excludes: Vec<String> = exclude_paths.iter().map(|p| normalize(p)).collect();

        // Run blocking I/O in a separate thread
        let result = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::create(&output)?;
            let encoder = GzEncoder::new(file, Compression::default());
            let mut archive = Builder::new(encoder);
            archive.follow_symlinks(false);

            let mut files = 0u64;
            for entry in walkdir::WalkDir::new(&source)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    let rel = e
                        .path()
                        .strip_prefix(&source)
                        .map(|p| format!("/{}", p.display()))
                        .unwrap_or_default();
                    keep_entry(&rel, e.file_type().is_dir(), &includes, &excludes)
                })
            {
                let entry = entry?;
                let path = entry.path();
                let rel_path = path.strip_prefix(&source)?;
                if rel_path.as_os_str().is_empty() {
                    continue;
                }
                let rel = format!("/{}", rel_path.display());
                if entry.file_type().is_dir() {
                    // A directory on the way to an include is not itself
                    // backed up; only what matches is.
                    if selected(&rel, &includes) {
                        archive.append_dir(rel_path, path)?;
                    }
                } else if selected(&rel, &includes) {
                    archive.append_path_with_name(path, rel_path)?;
                    files += 1;
                }
            }

            if !includes.is_empty() && files == 0 {
                anyhow::bail!("nothing matched the backup paths {:?}", includes);
            }

            // Finish the archive and get the encoder back
            let encoder = archive.into_inner()?;
            // Finish the gzip encoder and get the file handle back
            let mut file = encoder.finish()?;
            // Ensure all data is written to disk
            use std::io::Write;
            file.flush()?;
            file.sync_all()?;
            drop(file);

            // Calculate checksum
            let file = std::fs::File::open(&output)?;
            let mut hasher = Sha256::new();
            let mut reader = std::io::BufReader::new(file);
            std::io::copy(&mut reader, &mut hasher)?;
            let checksum = format!("{:x}", hasher.finalize());

            // Get file size
            let metadata = std::fs::metadata(&output)?;

            Ok::<_, anyhow::Error>((metadata.len(), checksum))
        })
        .await??;

        Ok(result)
    }

    /// List backups for a container
    pub async fn list_backups(&self, container_id: &str) -> Result<Vec<BackupInfo>> {
        let backups = self.backups.read().await;

        if let Some(container_backups) = backups.get(container_id) {
            let mut list: Vec<BackupInfo> = container_backups.values().cloned().collect();
            list.sort_by_key(|b| std::cmp::Reverse(b.created_at));
            Ok(list)
        } else {
            Ok(Vec::new())
        }
    }

    /// Get a specific backup
    pub async fn get_backup(&self, container_id: &str, backup_id: &str) -> Result<BackupInfo> {
        let backups = self.backups.read().await;

        backups
            .get(container_id)
            .and_then(|cb| cb.get(backup_id))
            .cloned()
            .ok_or_else(|| {
                NodeError::InvalidInput(format!(
                    "Backup {} not found for container {}",
                    backup_id, container_id
                ))
            })
    }

    /// Restore a backup.
    ///
    /// With `delete_existing`, the server directory is replaced by the
    /// archive's contents in one swap: the archive is unpacked beside it
    /// first, and nothing changes if that fails. Without it, the archive is
    /// unpacked over the directory, adding and overwriting files and leaving
    /// the rest alone.
    pub async fn restore_backup(
        &self,
        container_id: &str,
        backup_id: &str,
        delete_existing: bool,
    ) -> Result<()> {
        let backup = self.get_backup(container_id, backup_id).await?;

        if backup.status != BackupStatus::Completed {
            return Err(NodeError::InvalidInput(format!(
                "Backup {} is not complete (status: {:?})",
                backup_id, backup.status
            )));
        }

        let container_dir = self.data_dir.join(container_id);
        let backup_path = self.archive_path(container_id, backup_id);

        if !backup_path.exists() {
            return Err(NodeError::InvalidInput(format!(
                "Backup file not found: {}",
                backup_path.display()
            )));
        }

        // Verify checksum (a recovered archive has none on record).
        if !backup.checksum.is_empty() {
            let actual_checksum = self.calculate_checksum(&backup_path).await?;
            if actual_checksum != backup.checksum {
                return Err(NodeError::InvalidInput(format!(
                    "Backup checksum mismatch: expected {}, got {}",
                    backup.checksum, actual_checksum
                )));
            }
        }

        let owner = self.owner;
        if delete_existing {
            // Unpack beside the directory, on the same filesystem, then swap.
            let stamp = Uuid::new_v4().simple().to_string();
            let staging = self.data_dir.join(format!(".{}.restore-{}", container_id, stamp));
            let old = self.data_dir.join(format!(".{}.old-{}", container_id, stamp));
            fs::create_dir_all(&staging).await?;
            if let Err(e) = unpack(&backup_path, &staging, owner).await {
                let _ = fs::remove_dir_all(&staging).await;
                return Err(e);
            }
            let had_dir = container_dir.exists();
            if had_dir {
                fs::rename(&container_dir, &old).await?;
            }
            if let Err(e) = fs::rename(&staging, &container_dir).await {
                // Put the old directory back; the staging copy is dropped.
                if had_dir {
                    let _ = fs::rename(&old, &container_dir).await;
                }
                let _ = fs::remove_dir_all(&staging).await;
                return Err(e.into());
            }
            if had_dir {
                if let Err(e) = fs::remove_dir_all(&old).await {
                    warn!(
                        "Restored, but the previous files remain at {:?}: {}",
                        old, e
                    );
                }
            }
        } else {
            fs::create_dir_all(&container_dir).await?;
            unpack(&backup_path, &container_dir, owner).await?;
        }

        info!(
            "Restored backup {} for container {}",
            backup_id, container_id
        );

        Ok(())
    }

    /// Delete a backup
    pub async fn delete_backup(&self, container_id: &str, backup_id: &str) -> Result<()> {
        let backup_path = self.archive_path(container_id, backup_id);

        // Remove from registry
        {
            let mut backups = self.backups.write().await;
            if let Some(container_backups) = backups.get_mut(container_id) {
                container_backups.remove(backup_id);
            }
        }
        let _ = fs::remove_file(self.record_path(container_id, backup_id)).await;

        // Delete the file
        if backup_path.exists() {
            fs::remove_file(&backup_path).await?;
        }

        info!(
            "Deleted backup {} for container {}",
            backup_id, container_id
        );

        Ok(())
    }

    /// Drop every backup of a container: its archives, records and folder.
    pub async fn delete_all(&self, container_id: &str) -> Result<()> {
        self.backups.write().await.remove(container_id);
        let dir = self.backup_dir.join(container_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).await?;
        }
        Ok(())
    }

    /// Get the path to a backup file (for downloading)
    pub fn get_backup_path(&self, container_id: &str, backup_id: &str) -> PathBuf {
        self.archive_path(container_id, backup_id)
    }

    /// Calculate SHA256 checksum of a file
    async fn calculate_checksum(&self, path: &Path) -> Result<String> {
        let path = path.to_path_buf();
        let checksum = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&path)?;
            let mut hasher = Sha256::new();
            let mut reader = std::io::BufReader::new(file);
            std::io::copy(&mut reader, &mut hasher)?;
            Ok::<_, anyhow::Error>(format!("{:x}", hasher.finalize()))
        })
        .await??;

        Ok(checksum)
    }
}

/// How a server is backed up, from its blueprint's `backups` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupPolicy {
    /// Paths to include; empty means the whole directory.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Backups to keep; `None` or zero keeps all.
    pub retention: Option<u32>,
    /// A console command to send first while the server runs (`save-all`).
    pub pre_backup_command: Option<String>,
}

impl BackupPolicy {
    pub fn from_config(config: &nexus_config::GameConfig) -> Self {
        match &config.backups {
            Some(b) => Self {
                include: b.paths.clone(),
                exclude: b.exclude.clone(),
                retention: b.retention,
                pre_backup_command: b.pre_backup_command.clone().filter(|c| !c.trim().is_empty()),
            },
            None => Self::default(),
        }
    }
}

/// Back up a server the way its blueprint says: send the pre-backup
/// command if it is running (and give the game `settle` to flush), archive
/// the blueprint's paths (or `paths` when given), then apply retention.
/// This is what the Backups tab, the API and schedules all go through.
pub async fn backup_server(
    manager: &crate::container::ContainerManager,
    backups: &BackupManager,
    container_id: &str,
    name: &str,
    paths: Option<Vec<String>>,
    settle: std::time::Duration,
) -> Result<BackupInfo> {
    let state = manager.get_state(container_id).await?;
    let policy = manager
        .load_blueprint(container_id)
        .await
        .map(|c| BackupPolicy::from_config(&c))
        .unwrap_or_default();

    if state.status.is_running() {
        if let Some(cmd) = &policy.pre_backup_command {
            match manager.send_command(container_id, cmd).await {
                Ok(()) => {
                    info!("Sent pre-backup command {:?} to {}", cmd, container_id);
                    tokio::time::sleep(settle).await;
                }
                Err(e) => warn!(
                    "Pre-backup command {:?} for {} was not sent: {}",
                    cmd, container_id, e
                ),
            }
        }
    }

    let include = paths.filter(|p| !p.is_empty()).unwrap_or_else(|| policy.include.clone());
    let info = match backups.create_backup(container_id, name, &include, &policy.exclude).await {
        Ok(info) => {
            manager.notify(
                crate::notify::Notification::new(
                    "backup.completed",
                    crate::notify::Severity::Info,
                    format!("Backup of {} completed", state.name),
                    format!("{} ({} MiB).", name, info.size / (1024 * 1024)),
                )
                .for_server(container_id, &state.name),
            );
            info
        }
        Err(e) => {
            manager.notify(
                crate::notify::Notification::new(
                    "backup.failed",
                    crate::notify::Severity::Warning,
                    format!("Backup of {} failed", state.name),
                    format!("{}: {}", name, e),
                )
                .for_server(container_id, &state.name),
            );
            return Err(e);
        }
    };

    if let Some(keep) = policy.retention.filter(|k| *k > 0) {
        if let Err(e) = backups.enforce_retention(container_id, keep).await {
            warn!("Retention for {} was not applied: {}", container_id, e);
        }
    }
    Ok(info)
}

/// Unpack an archive into `dest`, refusing entries that would land outside
/// it, and hand the result to `owner`.
async fn unpack(archive: &Path, dest: &Path, owner: Option<(u32, u32)>) -> Result<()> {
    let source = archive.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&source)?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        archive.set_preserve_permissions(true);
        archive.set_overwrite(true);
        for entry in archive.entries()? {
            let mut entry = entry?;
            if !entry.unpack_in(&dest)? {
                warn!(
                    "Skipping backup entry outside the server directory: {:?}",
                    entry.path().unwrap_or_default()
                );
            }
        }
        if let Some((uid, gid)) = owner {
            for entry in walkdir::WalkDir::new(&dest).follow_links(false).into_iter().flatten() {
                if let Err(e) = std::os::unix::fs::lchown(entry.path(), Some(uid), Some(gid)) {
                    if e.kind() == std::io::ErrorKind::PermissionDenied {
                        break;
                    }
                    warn!("Could not chown {:?}: {}", entry.path(), e);
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

/// A backup path as written in a blueprint, made absolute within the
/// server directory with no trailing slash: `world/` and `/world` are the
/// same thing.
fn normalize(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    let trimmed = trimmed.trim_start_matches('/');
    format!("/{}", trimmed)
}

/// Whether `rel` (absolute within the server directory) is under or equal
/// to `prefix`, on a path-component boundary.
fn under(rel: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    rel == prefix || rel.starts_with(&format!("{}/", prefix))
}

/// Whether an entry matches an include (or there are none).
fn selected(rel: &str, includes: &[String]) -> bool {
    includes.is_empty() || includes.iter().any(|i| under(rel, i) || glob_match(i, rel))
}

/// The walk filter: skip excluded entries, and keep a directory when it is
/// selected or when something selected lies beneath it. A walk that
/// dropped `/` because `/world` did not match it would back up nothing,
/// which is what selective backups used to do.
pub fn keep_entry(rel: &str, is_dir: bool, includes: &[String], excludes: &[String]) -> bool {
    if excludes.iter().any(|x| under(rel, x) || glob_match(x, rel)) {
        return false;
    }
    if includes.is_empty() {
        return true;
    }
    if selected(rel, includes) {
        return true;
    }
    is_dir && includes.iter().any(|i| under(i, rel))
}

/// Simple glob pattern matching
fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            let (prefix, suffix) = (parts[0], parts[1]);
            return path.starts_with(prefix) && path.ends_with(suffix);
        }
    }
    pattern == path
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_backup_create_and_restore() {
        let temp = TempDir::new().unwrap();
        let manager = BackupManager::new(temp.path());
        manager.init().await.unwrap();

        // Create a test container directory with files
        let container_id = "test-container";
        let container_dir = temp.path().join(container_id);
        fs::create_dir_all(&container_dir).await.unwrap();
        fs::write(container_dir.join("test.txt"), "Hello, World!").await.unwrap();
        fs::create_dir(container_dir.join("subdir")).await.unwrap();
        fs::write(container_dir.join("subdir/nested.txt"), "Nested file").await.unwrap();

        // Create backup
        let backup = manager.create_backup(container_id, "Test Backup", &[], &[]).await.unwrap();

        assert_eq!(backup.status, BackupStatus::Completed);
        assert!(backup.size > 0);
        assert!(!backup.checksum.is_empty());

        // Delete the original files
        fs::remove_dir_all(&container_dir).await.unwrap();

        // Restore backup
        manager.restore_backup(container_id, &backup.id, false).await.unwrap();

        // Verify files are restored
        assert!(container_dir.join("test.txt").exists());
        assert!(container_dir.join("subdir/nested.txt").exists());
    }

    #[tokio::test]
    async fn test_list_backups() {
        let temp = TempDir::new().unwrap();
        let manager = BackupManager::new(temp.path());
        manager.init().await.unwrap();

        // Create a test container
        let container_id = "test-container";
        let container_dir = temp.path().join(container_id);
        fs::create_dir_all(&container_dir).await.unwrap();
        fs::write(container_dir.join("test.txt"), "Hello").await.unwrap();

        // Create multiple backups
        manager.create_backup(container_id, "Backup 1", &[], &[]).await.unwrap();
        manager.create_backup(container_id, "Backup 2", &[], &[]).await.unwrap();

        // List backups
        let backups = manager.list_backups(container_id).await.unwrap();
        assert_eq!(backups.len(), 2);
    }

    #[test]
    fn walk_filter_keeps_the_way_to_an_include() {
        let inc = vec!["/world".to_string(), "/plugins/*.jar".to_string()];
        let exc = vec!["/logs".to_string(), "/cache".to_string()];
        // The root and the directories above an include are walked.
        assert!(keep_entry("/", true, &inc, &exc));
        assert!(keep_entry("/plugins", true, &inc, &exc));
        // What matches is kept, whole.
        assert!(keep_entry("/world", true, &inc, &exc));
        assert!(keep_entry("/world/region/r.0.0.mca", false, &inc, &exc));
        assert!(keep_entry("/plugins/Essentials.jar", false, &inc, &exc));
        // Siblings are not.
        assert!(!keep_entry("/world_nether", true, &inc, &exc));
        assert!(!keep_entry(
            "/plugins/Essentials/config.yml",
            false,
            &inc,
            &exc
        ));
        assert!(!keep_entry("/server.jar", false, &inc, &exc));
        // Excludes win, even inside an include.
        assert!(!keep_entry("/logs", true, &inc, &exc));
        assert!(!keep_entry("/logs/latest.log", false, &inc, &exc));
        let exc2 = vec!["/world/session.lock".to_string()];
        assert!(!keep_entry("/world/session.lock", false, &inc, &exc2));
        // Only what is selected is written, not the directories walked
        // through to reach it.
        assert!(!selected("/plugins", &inc));
        assert!(selected("/world/level.dat", &inc));
        // No includes: everything but the excludes.
        assert!(keep_entry("/anything", false, &[], &exc));
        assert!(!keep_entry("/cache/x", false, &[], &exc));
        assert_eq!(normalize("world/"), "/world");
        assert_eq!(normalize("/world"), "/world");
    }

    #[tokio::test]
    async fn selective_backup_contains_only_the_selected_paths() {
        let temp = TempDir::new().unwrap();
        let manager = BackupManager::new(temp.path());
        manager.init().await.unwrap();
        let id = "srv";
        let dir = temp.path().join(id);
        fs::create_dir_all(dir.join("world/region")).await.unwrap();
        fs::create_dir_all(dir.join("logs")).await.unwrap();
        fs::write(dir.join("world/level.dat"), "lvl").await.unwrap();
        fs::write(dir.join("world/region/r.mca"), "chunk").await.unwrap();
        fs::write(dir.join("world/session.lock"), "x").await.unwrap();
        fs::write(dir.join("server.jar"), "jar").await.unwrap();
        fs::write(dir.join("logs/latest.log"), "log").await.unwrap();

        let backup = manager
            .create_backup(
                id,
                "world only",
                &["world".to_string()],
                &["/world/session.lock".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(backup.include, vec!["/world"]);

        // Restore into an empty directory and see what came back.
        fs::remove_dir_all(&dir).await.unwrap();
        manager.restore_backup(id, &backup.id, true).await.unwrap();
        assert!(dir.join("world/level.dat").exists());
        assert!(dir.join("world/region/r.mca").exists());
        assert!(!dir.join("world/session.lock").exists());
        assert!(!dir.join("server.jar").exists());
        assert!(!dir.join("logs").exists());

        // Paths that match nothing are an error, not an empty archive.
        let err = manager
            .create_backup(id, "nothing", &["/no-such-dir".to_string()], &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nothing matched"), "{}", err);
        let failed = manager.list_backups(id).await.unwrap();
        assert!(failed.iter().any(|b| b.status == BackupStatus::Failed));
    }

    #[tokio::test]
    async fn records_survive_a_restart_and_orphans_are_adopted() {
        let temp = TempDir::new().unwrap();
        let id = "srv";
        let dir = temp.path().join(id);
        fs::create_dir_all(&dir).await.unwrap();
        fs::write(dir.join("a.txt"), "a").await.unwrap();

        let backup = {
            let manager = BackupManager::new(temp.path());
            manager.init().await.unwrap();
            manager.create_backup(id, "kept", &[], &[]).await.unwrap()
        };
        // An archive dropped in by hand, with no record.
        let orphan = temp.path().join("backups").join(id).join("orphan.tar.gz");
        fs::copy(
            temp.path().join("backups").join(id).join(format!("{}.tar.gz", backup.id)),
            &orphan,
        )
        .await
        .unwrap();
        // A record whose archive is gone.
        let ghost = BackupInfo {
            id: "ghost".into(),
            container_id: id.into(),
            name: "ghost".into(),
            size: 1,
            created_at: 1,
            checksum: "x".into(),
            status: BackupStatus::Completed,
            error: None,
            include: vec![],
            exclude: vec![],
        };
        fs::write(
            temp.path().join("backups").join(id).join("ghost.json"),
            serde_json::to_vec(&ghost).unwrap(),
        )
        .await
        .unwrap();

        let manager = BackupManager::new(temp.path());
        manager.init().await.unwrap();
        let list = manager.list_backups(id).await.unwrap();
        let ids: Vec<&str> = list.iter().map(|b| b.id.as_str()).collect();
        assert!(ids.contains(&backup.id.as_str()), "{:?}", ids);
        assert!(ids.contains(&"orphan"), "{:?}", ids);
        assert!(!ids.contains(&"ghost"), "{:?}", ids);
        let kept = manager.get_backup(id, &backup.id).await.unwrap();
        assert_eq!(kept.name, "kept");
        assert_eq!(kept.checksum, backup.checksum);
        // The adopted archive restores without a checksum to compare.
        fs::write(dir.join("a.txt"), "changed").await.unwrap();
        manager.restore_backup(id, "orphan", true).await.unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).await.unwrap(), "a");
    }

    #[tokio::test]
    async fn retention_keeps_the_newest() {
        let temp = TempDir::new().unwrap();
        let manager = BackupManager::new(temp.path());
        manager.init().await.unwrap();
        let id = "srv";
        fs::create_dir_all(temp.path().join(id)).await.unwrap();
        fs::write(temp.path().join(id).join("f"), "x").await.unwrap();
        let mut made = Vec::new();
        for i in 0..4 {
            let mut b = manager.create_backup(id, &format!("b{}", i), &[], &[]).await.unwrap();
            // Space them out in time deterministically.
            b.created_at = 1000 + i;
            manager.register(&b).await;
            made.push(b.id);
        }
        let removed = manager.enforce_retention(id, 2).await.unwrap();
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&made[0]) && removed.contains(&made[1]));
        let left = manager.list_backups(id).await.unwrap();
        assert_eq!(left.len(), 2);
        assert!(!temp
            .path()
            .join("backups")
            .join(id)
            .join(format!("{}.tar.gz", made[0]))
            .exists());
        assert!(!temp.path().join("backups").join(id).join(format!("{}.json", made[0])).exists());
        assert!(manager.enforce_retention(id, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_swap_restore_leaves_the_server_untouched() {
        let temp = TempDir::new().unwrap();
        let manager = BackupManager::new(temp.path());
        manager.init().await.unwrap();
        let id = "srv";
        let dir = temp.path().join(id);
        fs::create_dir_all(&dir).await.unwrap();
        fs::write(dir.join("keep.txt"), "original").await.unwrap();
        let backup = manager.create_backup(id, "b", &[], &[]).await.unwrap();

        // Corrupt the archive: the checksum no longer matches.
        let archive = manager.get_backup_path(id, &backup.id);
        fs::write(&archive, b"garbage").await.unwrap();
        fs::write(dir.join("keep.txt"), "still here").await.unwrap();
        let err = manager.restore_backup(id, &backup.id, true).await.unwrap_err();
        assert!(err.to_string().contains("checksum"), "{}", err);
        assert_eq!(
            fs::read_to_string(dir.join("keep.txt")).await.unwrap(),
            "still here"
        );
        // No staging or old directories left around.
        let stray: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(stray.is_empty(), "{:?}", stray);
    }
}

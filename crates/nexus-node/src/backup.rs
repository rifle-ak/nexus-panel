//! Backup management for container server data
//!
//! Provides functionality to create, restore, and manage backups of container data:
//! - Compressed tar.gz backups
//! - SHA256 checksum verification
//! - Include/exclude path patterns
//! - Background backup creation

use crate::error::{NodeError, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Backup information
#[derive(Debug, Clone)]
pub struct BackupInfo {
    pub id: String,
    pub container_id: String,
    pub name: String,
    pub size: u64,
    pub created_at: i64,
    pub checksum: String,
    pub status: BackupStatus,
    pub error: Option<String>,
}

/// Backup manager for containers
pub struct BackupManager {
    /// Base data directory
    data_dir: PathBuf,

    /// Backup storage directory
    backup_dir: PathBuf,

    /// In-memory backup registry (production would use a database)
    backups: Arc<RwLock<HashMap<String, HashMap<String, BackupInfo>>>>,
}

impl BackupManager {
    /// Create a new backup manager
    pub fn new(data_dir: &Path) -> Self {
        let backup_dir = data_dir.join("backups");
        Self {
            data_dir: data_dir.to_path_buf(),
            backup_dir,
            backups: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Initialize backup storage
    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.backup_dir).await?;
        Ok(())
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

        let backup_path = container_backup_dir.join(format!("{}.tar.gz", backup_id));

        // Create initial backup info
        let mut backup_info = BackupInfo {
            id: backup_id.clone(),
            container_id: container_id.to_string(),
            name: name.to_string(),
            size: 0,
            created_at: chrono::Utc::now().timestamp(),
            checksum: String::new(),
            status: BackupStatus::InProgress,
            error: None,
        };

        // Register the backup
        {
            let mut backups = self.backups.write().await;
            let container_backups =
                backups.entry(container_id.to_string()).or_insert_with(HashMap::new);
            container_backups.insert(backup_id.clone(), backup_info.clone());
        }

        // Create the backup
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

        // Update backup info
        {
            let mut backups = self.backups.write().await;
            if let Some(container_backups) = backups.get_mut(container_id) {
                container_backups.insert(backup_id.clone(), backup_info.clone());
            }
        }

        if backup_info.status == BackupStatus::Failed {
            // Clean up failed backup file
            let _ = fs::remove_file(&backup_path).await;
            return Err(NodeError::Internal(
                backup_info.error.unwrap_or_else(|| "Backup failed".to_string()),
            ));
        }

        Ok(backup_info)
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
        let includes: Vec<String> = include_paths.to_vec();
        let excludes: Vec<String> = exclude_paths.to_vec();

        // Run blocking I/O in a separate thread
        let result = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::create(&output)?;
            let encoder = GzEncoder::new(file, Compression::default());
            let mut archive = Builder::new(encoder);

            // Walk the directory
            for entry in walkdir::WalkDir::new(&source).into_iter().filter_entry(|e| {
                let path = e
                    .path()
                    .strip_prefix(&source)
                    .map(|p| format!("/{}", p.display()))
                    .unwrap_or_default();

                // Check excludes
                for exclude in &excludes {
                    if path.starts_with(exclude) || glob_match(exclude, &path) {
                        return false;
                    }
                }

                // Check includes (if specified)
                if !includes.is_empty() {
                    for include in &includes {
                        if path.starts_with(include) || glob_match(include, &path) {
                            return true;
                        }
                    }
                    return false;
                }

                true
            }) {
                let entry = entry?;
                let path = entry.path();
                let rel_path = path.strip_prefix(&source)?;

                if path.is_file() {
                    archive.append_path_with_name(path, rel_path)?;
                } else if path.is_dir() && !rel_path.as_os_str().is_empty() {
                    archive.append_dir(rel_path, path)?;
                }
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

    /// Restore a backup
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
        let backup_path = self.backup_dir.join(container_id).join(format!("{}.tar.gz", backup_id));

        if !backup_path.exists() {
            return Err(NodeError::InvalidInput(format!(
                "Backup file not found: {}",
                backup_path.display()
            )));
        }

        // Verify checksum
        let actual_checksum = self.calculate_checksum(&backup_path).await?;
        if actual_checksum != backup.checksum {
            return Err(NodeError::InvalidInput(format!(
                "Backup checksum mismatch: expected {}, got {}",
                backup.checksum, actual_checksum
            )));
        }

        // Delete existing data if requested
        if delete_existing && container_dir.exists() {
            fs::remove_dir_all(&container_dir).await?;
        }

        // Create container directory
        fs::create_dir_all(&container_dir).await?;

        // Extract the backup
        let source = backup_path.clone();
        let dest = container_dir.clone();

        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&source)?;
            let decoder = GzDecoder::new(file);
            let mut archive = Archive::new(decoder);
            archive.unpack(&dest)?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;

        info!(
            "Restored backup {} for container {}",
            backup_id, container_id
        );

        Ok(())
    }

    /// Delete a backup
    pub async fn delete_backup(&self, container_id: &str, backup_id: &str) -> Result<()> {
        let backup_path = self.backup_dir.join(container_id).join(format!("{}.tar.gz", backup_id));

        // Remove from registry
        {
            let mut backups = self.backups.write().await;
            if let Some(container_backups) = backups.get_mut(container_id) {
                container_backups.remove(backup_id);
            }
        }

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

    /// Get the path to a backup file (for downloading)
    pub fn get_backup_path(&self, container_id: &str, backup_id: &str) -> PathBuf {
        self.backup_dir.join(container_id).join(format!("{}.tar.gz", backup_id))
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
}

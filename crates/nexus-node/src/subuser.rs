//! Subuser management for game servers
//!
//! Provides multi-tenant access control for server management:
//! - Create, update, delete subusers
//! - Permission-based access control
//! - Audit logging for subuser actions

use crate::error::{NodeError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Available permissions for subusers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    // Power permissions
    PowerStart,
    PowerStop,
    PowerRestart,
    PowerKill,

    // Console permissions
    ConsoleRead,
    ConsoleWrite,

    // File permissions
    FileRead,
    FileWrite,
    FileDelete,
    FileArchive,
    FileSftp,

    // Backup permissions
    BackupCreate,
    BackupRead,
    BackupDelete,
    BackupRestore,
    BackupDownload,

    // Database permissions
    DatabaseCreate,
    DatabaseRead,
    DatabaseUpdate,
    DatabaseDelete,

    // Schedule permissions
    ScheduleCreate,
    ScheduleRead,
    ScheduleUpdate,
    ScheduleDelete,

    // Settings permissions
    SettingsRead,
    SettingsRename,
    SettingsReinstall,

    // User management (admin only)
    UserCreate,
    UserRead,
    UserUpdate,
    UserDelete,

    // Allocation permissions
    AllocationRead,
    AllocationCreate,
    AllocationUpdate,
    AllocationDelete,

    // Activity log
    ActivityRead,

    // Websocket
    WebsocketConnect,
}

impl Permission {
    /// Get all available permissions
    pub fn all() -> HashSet<Permission> {
        use Permission::*;
        [
            PowerStart, PowerStop, PowerRestart, PowerKill,
            ConsoleRead, ConsoleWrite,
            FileRead, FileWrite, FileDelete, FileArchive, FileSftp,
            BackupCreate, BackupRead, BackupDelete, BackupRestore, BackupDownload,
            DatabaseCreate, DatabaseRead, DatabaseUpdate, DatabaseDelete,
            ScheduleCreate, ScheduleRead, ScheduleUpdate, ScheduleDelete,
            SettingsRead, SettingsRename, SettingsReinstall,
            UserCreate, UserRead, UserUpdate, UserDelete,
            AllocationRead, AllocationCreate, AllocationUpdate, AllocationDelete,
            ActivityRead, WebsocketConnect,
        ].into_iter().collect()
    }

    /// Get default permissions for a new subuser
    pub fn default_permissions() -> HashSet<Permission> {
        use Permission::*;
        [
            ConsoleRead, ConsoleWrite,
            FileRead, FileWrite,
            BackupRead,
            ScheduleRead,
            SettingsRead,
            ActivityRead,
            WebsocketConnect,
        ].into_iter().collect()
    }

    /// Get read-only permissions
    pub fn read_only() -> HashSet<Permission> {
        use Permission::*;
        [
            ConsoleRead,
            FileRead,
            BackupRead,
            DatabaseRead,
            ScheduleRead,
            SettingsRead,
            UserRead,
            AllocationRead,
            ActivityRead,
        ].into_iter().collect()
    }

    /// Get operator permissions (most operations except admin)
    pub fn operator() -> HashSet<Permission> {
        use Permission::*;
        [
            PowerStart, PowerStop, PowerRestart,
            ConsoleRead, ConsoleWrite,
            FileRead, FileWrite, FileDelete, FileArchive, FileSftp,
            BackupCreate, BackupRead, BackupDelete, BackupRestore, BackupDownload,
            DatabaseCreate, DatabaseRead, DatabaseUpdate, DatabaseDelete,
            ScheduleCreate, ScheduleRead, ScheduleUpdate, ScheduleDelete,
            SettingsRead,
            AllocationRead,
            ActivityRead, WebsocketConnect,
        ].into_iter().collect()
    }

    /// Convert permission to string
    pub fn as_str(&self) -> &'static str {
        use Permission::*;
        match self {
            PowerStart => "power.start",
            PowerStop => "power.stop",
            PowerRestart => "power.restart",
            PowerKill => "power.kill",
            ConsoleRead => "console.read",
            ConsoleWrite => "console.write",
            FileRead => "file.read",
            FileWrite => "file.write",
            FileDelete => "file.delete",
            FileArchive => "file.archive",
            FileSftp => "file.sftp",
            BackupCreate => "backup.create",
            BackupRead => "backup.read",
            BackupDelete => "backup.delete",
            BackupRestore => "backup.restore",
            BackupDownload => "backup.download",
            DatabaseCreate => "database.create",
            DatabaseRead => "database.read",
            DatabaseUpdate => "database.update",
            DatabaseDelete => "database.delete",
            ScheduleCreate => "schedule.create",
            ScheduleRead => "schedule.read",
            ScheduleUpdate => "schedule.update",
            ScheduleDelete => "schedule.delete",
            SettingsRead => "settings.read",
            SettingsRename => "settings.rename",
            SettingsReinstall => "settings.reinstall",
            UserCreate => "user.create",
            UserRead => "user.read",
            UserUpdate => "user.update",
            UserDelete => "user.delete",
            AllocationRead => "allocation.read",
            AllocationCreate => "allocation.create",
            AllocationUpdate => "allocation.update",
            AllocationDelete => "allocation.delete",
            ActivityRead => "activity.read",
            WebsocketConnect => "websocket.connect",
        }
    }

    /// Parse permission from string
    pub fn from_str(s: &str) -> Option<Permission> {
        use Permission::*;
        match s {
            "power.start" => Some(PowerStart),
            "power.stop" => Some(PowerStop),
            "power.restart" => Some(PowerRestart),
            "power.kill" => Some(PowerKill),
            "console.read" => Some(ConsoleRead),
            "console.write" => Some(ConsoleWrite),
            "file.read" => Some(FileRead),
            "file.write" => Some(FileWrite),
            "file.delete" => Some(FileDelete),
            "file.archive" => Some(FileArchive),
            "file.sftp" => Some(FileSftp),
            "backup.create" => Some(BackupCreate),
            "backup.read" => Some(BackupRead),
            "backup.delete" => Some(BackupDelete),
            "backup.restore" => Some(BackupRestore),
            "backup.download" => Some(BackupDownload),
            "database.create" => Some(DatabaseCreate),
            "database.read" => Some(DatabaseRead),
            "database.update" => Some(DatabaseUpdate),
            "database.delete" => Some(DatabaseDelete),
            "schedule.create" => Some(ScheduleCreate),
            "schedule.read" => Some(ScheduleRead),
            "schedule.update" => Some(ScheduleUpdate),
            "schedule.delete" => Some(ScheduleDelete),
            "settings.read" => Some(SettingsRead),
            "settings.rename" => Some(SettingsRename),
            "settings.reinstall" => Some(SettingsReinstall),
            "user.create" => Some(UserCreate),
            "user.read" => Some(UserRead),
            "user.update" => Some(UserUpdate),
            "user.delete" => Some(UserDelete),
            "allocation.read" => Some(AllocationRead),
            "allocation.create" => Some(AllocationCreate),
            "allocation.update" => Some(AllocationUpdate),
            "allocation.delete" => Some(AllocationDelete),
            "activity.read" => Some(ActivityRead),
            "websocket.connect" => Some(WebsocketConnect),
            _ => None,
        }
    }
}

/// Subuser information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subuser {
    /// Unique subuser ID
    pub id: String,
    /// User ID from external system (e.g., panel user ID)
    pub user_id: String,
    /// Container/server ID this subuser has access to
    pub container_id: String,
    /// Email address
    pub email: String,
    /// Display name
    pub name: Option<String>,
    /// Granted permissions
    pub permissions: HashSet<Permission>,
    /// When the subuser was created
    pub created_at: DateTime<Utc>,
    /// When the subuser was last updated
    pub updated_at: DateTime<Utc>,
    /// SFTP password hash (if SFTP access is enabled)
    pub sftp_password_hash: Option<String>,
}

impl Subuser {
    /// Check if subuser has a specific permission
    pub fn has_permission(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }

    /// Check if subuser has all specified permissions
    pub fn has_all_permissions(&self, permissions: &[Permission]) -> bool {
        permissions.iter().all(|p| self.permissions.contains(p))
    }

    /// Check if subuser has any of the specified permissions
    pub fn has_any_permission(&self, permissions: &[Permission]) -> bool {
        permissions.iter().any(|p| self.permissions.contains(p))
    }

    /// Set SFTP password
    pub fn set_sftp_password(&mut self, password: &str) {
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        self.sftp_password_hash = Some(format!("{:x}", hasher.finalize()));
    }

    /// Verify SFTP password
    pub fn verify_sftp_password(&self, password: &str) -> bool {
        if let Some(ref hash) = self.sftp_password_hash {
            let mut hasher = Sha256::new();
            hasher.update(password.as_bytes());
            let computed = format!("{:x}", hasher.finalize());
            computed == *hash
        } else {
            false
        }
    }
}

/// Subuser manager for handling subuser operations
pub struct SubuserManager {
    /// Subusers indexed by container_id -> subuser_id -> Subuser
    subusers: Arc<RwLock<HashMap<String, HashMap<String, Subuser>>>>,
    /// Index by user_id for quick lookup
    user_index: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl SubuserManager {
    /// Create a new subuser manager
    pub fn new() -> Self {
        Self {
            subusers: Arc::new(RwLock::new(HashMap::new())),
            user_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new subuser
    pub async fn create_subuser(
        &self,
        container_id: &str,
        user_id: &str,
        email: &str,
        name: Option<&str>,
        permissions: HashSet<Permission>,
    ) -> Result<Subuser> {
        let subuser_id = Uuid::new_v4().to_string();
        let now = Utc::now();

        let subuser = Subuser {
            id: subuser_id.clone(),
            user_id: user_id.to_string(),
            container_id: container_id.to_string(),
            email: email.to_string(),
            name: name.map(|s| s.to_string()),
            permissions,
            created_at: now,
            updated_at: now,
            sftp_password_hash: None,
        };

        // Store subuser
        {
            let mut subusers = self.subusers.write().await;
            let container_subusers = subusers
                .entry(container_id.to_string())
                .or_insert_with(HashMap::new);
            container_subusers.insert(subuser_id.clone(), subuser.clone());
        }

        // Update user index
        {
            let mut user_index = self.user_index.write().await;
            user_index
                .entry(user_id.to_string())
                .or_insert_with(Vec::new)
                .push(subuser_id.clone());
        }

        info!(
            "Created subuser {} for container {} (user: {})",
            subuser_id, container_id, user_id
        );

        Ok(subuser)
    }

    /// Get a subuser by ID
    pub async fn get_subuser(&self, container_id: &str, subuser_id: &str) -> Result<Subuser> {
        let subusers = self.subusers.read().await;

        subusers
            .get(container_id)
            .and_then(|cs| cs.get(subuser_id))
            .cloned()
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "Subuser {} not found for container {}",
                subuser_id, container_id
            )))
    }

    /// List all subusers for a container
    pub async fn list_subusers(&self, container_id: &str) -> Result<Vec<Subuser>> {
        let subusers = self.subusers.read().await;

        if let Some(container_subusers) = subusers.get(container_id) {
            let mut list: Vec<Subuser> = container_subusers.values().cloned().collect();
            list.sort_by(|a, b| a.email.cmp(&b.email));
            Ok(list)
        } else {
            Ok(Vec::new())
        }
    }

    /// List all containers a user has access to
    pub async fn list_user_containers(&self, user_id: &str) -> Result<Vec<String>> {
        let user_index = self.user_index.read().await;
        let subusers = self.subusers.read().await;

        let mut containers = HashSet::new();

        if let Some(subuser_ids) = user_index.get(user_id) {
            for (container_id, container_subusers) in subusers.iter() {
                for subuser_id in subuser_ids {
                    if container_subusers.contains_key(subuser_id) {
                        containers.insert(container_id.clone());
                    }
                }
            }
        }

        Ok(containers.into_iter().collect())
    }

    /// Update subuser permissions
    pub async fn update_permissions(
        &self,
        container_id: &str,
        subuser_id: &str,
        permissions: HashSet<Permission>,
    ) -> Result<Subuser> {
        let mut subusers = self.subusers.write().await;

        let container_subusers = subusers
            .get_mut(container_id)
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "No subusers found for container {}",
                container_id
            )))?;

        let subuser = container_subusers
            .get_mut(subuser_id)
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "Subuser {} not found",
                subuser_id
            )))?;

        subuser.permissions = permissions;
        subuser.updated_at = Utc::now();

        info!(
            "Updated permissions for subuser {} on container {}",
            subuser_id, container_id
        );

        Ok(subuser.clone())
    }

    /// Delete a subuser
    pub async fn delete_subuser(&self, container_id: &str, subuser_id: &str) -> Result<()> {
        // Get user_id before removing
        let user_id = {
            let subusers = self.subusers.read().await;
            subusers
                .get(container_id)
                .and_then(|cs| cs.get(subuser_id))
                .map(|s| s.user_id.clone())
        };

        // Remove from subusers
        {
            let mut subusers = self.subusers.write().await;
            if let Some(container_subusers) = subusers.get_mut(container_id) {
                if container_subusers.remove(subuser_id).is_none() {
                    return Err(NodeError::InvalidInput(format!(
                        "Subuser {} not found for container {}",
                        subuser_id, container_id
                    )));
                }
            } else {
                return Err(NodeError::InvalidInput(format!(
                    "No subusers found for container {}",
                    container_id
                )));
            }
        }

        // Remove from user index
        if let Some(user_id) = user_id {
            let mut user_index = self.user_index.write().await;
            if let Some(subuser_ids) = user_index.get_mut(&user_id) {
                subuser_ids.retain(|id| id != subuser_id);
            }
        }

        info!(
            "Deleted subuser {} from container {}",
            subuser_id, container_id
        );

        Ok(())
    }

    /// Check if a user has a specific permission on a container
    pub async fn check_permission(
        &self,
        container_id: &str,
        user_id: &str,
        permission: Permission,
    ) -> bool {
        let subusers = self.subusers.read().await;

        if let Some(container_subusers) = subusers.get(container_id) {
            for subuser in container_subusers.values() {
                if subuser.user_id == user_id && subuser.has_permission(permission) {
                    return true;
                }
            }
        }

        false
    }

    /// Get subuser by user_id and container_id
    pub async fn get_subuser_by_user(
        &self,
        container_id: &str,
        user_id: &str,
    ) -> Option<Subuser> {
        let subusers = self.subusers.read().await;

        if let Some(container_subusers) = subusers.get(container_id) {
            for subuser in container_subusers.values() {
                if subuser.user_id == user_id {
                    return Some(subuser.clone());
                }
            }
        }

        None
    }

    /// Set SFTP password for a subuser
    pub async fn set_sftp_password(
        &self,
        container_id: &str,
        subuser_id: &str,
        password: &str,
    ) -> Result<()> {
        let mut subusers = self.subusers.write().await;

        let container_subusers = subusers
            .get_mut(container_id)
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "No subusers found for container {}",
                container_id
            )))?;

        let subuser = container_subusers
            .get_mut(subuser_id)
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "Subuser {} not found",
                subuser_id
            )))?;

        // Check if subuser has SFTP permission
        if !subuser.has_permission(Permission::FileSftp) {
            return Err(NodeError::InvalidInput(
                "Subuser does not have SFTP permission".to_string()
            ));
        }

        subuser.set_sftp_password(password);
        subuser.updated_at = Utc::now();

        info!(
            "Set SFTP password for subuser {} on container {}",
            subuser_id, container_id
        );

        Ok(())
    }

    /// Verify SFTP credentials
    pub async fn verify_sftp_credentials(
        &self,
        container_id: &str,
        username: &str,
        password: &str,
    ) -> Option<Subuser> {
        let subusers = self.subusers.read().await;

        if let Some(container_subusers) = subusers.get(container_id) {
            for subuser in container_subusers.values() {
                // Username could be subuser_id, user_id, or email
                if (subuser.id == username || subuser.user_id == username || subuser.email == username)
                    && subuser.has_permission(Permission::FileSftp)
                    && subuser.verify_sftp_password(password)
                {
                    return Some(subuser.clone());
                }
            }
        }

        None
    }
}

impl Default for SubuserManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_all() {
        let all = Permission::all();
        assert!(all.len() > 30); // Should have many permissions
    }

    #[test]
    fn test_permission_as_str() {
        assert_eq!(Permission::PowerStart.as_str(), "power.start");
        assert_eq!(Permission::FileRead.as_str(), "file.read");
    }

    #[test]
    fn test_permission_from_str() {
        assert_eq!(Permission::from_str("power.start"), Some(Permission::PowerStart));
        assert_eq!(Permission::from_str("invalid"), None);
    }

    #[tokio::test]
    async fn test_create_and_get_subuser() {
        let manager = SubuserManager::new();

        let permissions = Permission::default_permissions();
        let subuser = manager
            .create_subuser(
                "container-1",
                "user-123",
                "test@example.com",
                Some("Test User"),
                permissions.clone(),
            )
            .await
            .unwrap();

        assert_eq!(subuser.email, "test@example.com");
        assert_eq!(subuser.permissions, permissions);

        // Get subuser
        let retrieved = manager
            .get_subuser("container-1", &subuser.id)
            .await
            .unwrap();
        assert_eq!(retrieved.id, subuser.id);
    }

    #[tokio::test]
    async fn test_update_permissions() {
        let manager = SubuserManager::new();

        let subuser = manager
            .create_subuser(
                "container-1",
                "user-123",
                "test@example.com",
                None,
                Permission::read_only(),
            )
            .await
            .unwrap();

        // Update to operator permissions
        let updated = manager
            .update_permissions("container-1", &subuser.id, Permission::operator())
            .await
            .unwrap();

        assert!(updated.has_permission(Permission::PowerStart));
        assert!(updated.has_permission(Permission::ConsoleWrite));
    }

    #[tokio::test]
    async fn test_delete_subuser() {
        let manager = SubuserManager::new();

        let subuser = manager
            .create_subuser(
                "container-1",
                "user-123",
                "test@example.com",
                None,
                Permission::default_permissions(),
            )
            .await
            .unwrap();

        // Delete
        manager.delete_subuser("container-1", &subuser.id).await.unwrap();

        // Should not be found
        assert!(manager.get_subuser("container-1", &subuser.id).await.is_err());
    }

    #[tokio::test]
    async fn test_sftp_password() {
        let manager = SubuserManager::new();

        let mut permissions = Permission::default_permissions();
        permissions.insert(Permission::FileSftp);

        let subuser = manager
            .create_subuser(
                "container-1",
                "user-123",
                "test@example.com",
                None,
                permissions,
            )
            .await
            .unwrap();

        // Set password
        manager
            .set_sftp_password("container-1", &subuser.id, "secret123")
            .await
            .unwrap();

        // Verify
        let verified = manager
            .verify_sftp_credentials("container-1", &subuser.id, "secret123")
            .await;
        assert!(verified.is_some());

        // Wrong password
        let wrong = manager
            .verify_sftp_credentials("container-1", &subuser.id, "wrongpass")
            .await;
        assert!(wrong.is_none());
    }
}

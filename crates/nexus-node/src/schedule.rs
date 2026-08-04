//! Schedule management for automated container tasks
//!
//! Provides cron-based scheduling for container operations:
//! - Console commands
//! - Power actions (start, stop, restart)
//! - Backup creation

use crate::backup::BackupManager;
use crate::container::ContainerManager;
use crate::error::{NodeError, Result};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Subdirectory of `DATA_DIR` where schedules are persisted.
const SCHEDULE_SUBDIR: &str = ".nexus/schedules";

/// Schedule task type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleTaskType {
    Command,
    Power,
    Backup,
}

impl ScheduleTaskType {
    pub fn from_i32(value: i32) -> Self {
        match value {
            1 => ScheduleTaskType::Command,
            2 => ScheduleTaskType::Power,
            3 => ScheduleTaskType::Backup,
            _ => ScheduleTaskType::Command,
        }
    }

    pub fn to_i32(&self) -> i32 {
        match self {
            ScheduleTaskType::Command => 1,
            ScheduleTaskType::Power => 2,
            ScheduleTaskType::Backup => 3,
        }
    }
}

/// Schedule task definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleTask {
    pub task_type: ScheduleTaskType,
    pub time_offset: u32, // Seconds after schedule triggers
    pub payload: String,  // Task-specific payload
}

/// Schedule information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleInfo {
    pub id: String,
    pub container_id: String,
    pub name: String,
    pub cron_expression: String,
    pub is_active: bool,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub tasks: Vec<ScheduleTask>,
}

/// Schedule execution callback
pub type ScheduleCallback = Arc<
    dyn Fn(&str, &ScheduleTask) -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync,
>;

/// Build the callback that dispatches schedule tasks to real node subsystems
/// (container commands / power actions / backups). Shared by the background
/// runner and the manual-trigger endpoint so both do the same thing.
pub fn dispatch_callback(
    manager: Arc<ContainerManager>,
    backup_manager: Arc<BackupManager>,
) -> ScheduleCallback {
    Arc::new(move |container_id: &str, task: &ScheduleTask| {
        let manager = manager.clone();
        let backup_manager = backup_manager.clone();
        let container_id = container_id.to_string();
        let task = task.clone();
        Box::pin(async move {
            match task.task_type {
                ScheduleTaskType::Command => {
                    manager.send_command(&container_id, &task.payload).await
                }
                ScheduleTaskType::Power => match task.payload.trim().to_lowercase().as_str() {
                    "start" => manager.start_container(&container_id).await,
                    "stop" => manager.stop_container(&container_id, None).await,
                    "restart" => manager.restart_container(&container_id).await,
                    other => Err(NodeError::InvalidInput(format!(
                        "Unknown power action: {}",
                        other
                    ))),
                },
                ScheduleTaskType::Backup => {
                    let name = if task.payload.is_empty() {
                        format!("scheduled-{}", chrono::Utc::now().timestamp())
                    } else {
                        task.payload.clone()
                    };
                    backup_manager.create_backup(&container_id, &name, &[], &[]).await.map(|_| ())
                }
            }
        })
    })
}

/// Schedule manager
pub struct ScheduleManager {
    /// Schedules by container_id -> schedule_id -> ScheduleInfo
    schedules: Arc<RwLock<HashMap<String, HashMap<String, ScheduleInfo>>>>,

    /// Shutdown signal
    shutdown: Arc<RwLock<bool>>,

    /// Data directory for persistence (None = in-memory only, e.g. tests).
    data_dir: Option<PathBuf>,
}

impl ScheduleManager {
    /// Create a new in-memory schedule manager (no persistence).
    pub fn new() -> Self {
        Self {
            schedules: Arc::new(RwLock::new(HashMap::new())),
            shutdown: Arc::new(RwLock::new(false)),
            data_dir: None,
        }
    }

    /// Create a schedule manager that persists schedules under `data_dir`.
    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            schedules: Arc::new(RwLock::new(HashMap::new())),
            shutdown: Arc::new(RwLock::new(false)),
            data_dir: Some(data_dir),
        }
    }

    // ── Persistence ──────────────────────────────────────────────────────

    fn schedule_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join(SCHEDULE_SUBDIR))
    }

    /// Write a schedule to disk (best-effort; failures are logged, not fatal).
    async fn persist(&self, schedule: &ScheduleInfo) {
        let Some(dir) = self.schedule_dir() else {
            return;
        };
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!("Failed to create schedule dir {:?}: {}", dir, e);
            return;
        }
        match serde_json::to_vec_pretty(schedule) {
            Ok(bytes) => {
                let path = dir.join(format!("{}.json", schedule.id));
                if let Err(e) = tokio::fs::write(&path, bytes).await {
                    warn!("Failed to persist schedule {}: {}", schedule.id, e);
                }
            }
            Err(e) => warn!("Failed to serialize schedule {}: {}", schedule.id, e),
        }
    }

    /// Remove a schedule's persisted file.
    async fn remove_persisted(&self, schedule_id: &str) {
        if let Some(dir) = self.schedule_dir() {
            let _ = tokio::fs::remove_file(dir.join(format!("{}.json", schedule_id))).await;
        }
    }

    /// Load persisted schedules from disk on startup, recomputing the next run
    /// time from each cron expression. Returns the number restored.
    pub async fn restore(&self) -> usize {
        let Some(dir) = self.schedule_dir() else {
            return 0;
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return 0,
        };

        let mut restored = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    warn!("Failed to read schedule file {:?}: {}", path, e);
                    continue;
                }
            };
            let mut schedule: ScheduleInfo = match serde_json::from_slice(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    warn!("Skipping unreadable schedule file {:?}: {}", path, e);
                    continue;
                }
            };

            // Recompute the next run from the cron expression.
            schedule.next_run_at = if schedule.is_active {
                Schedule::from_str(&schedule.cron_expression)
                    .ok()
                    .and_then(|s| s.upcoming(chrono::Utc).next())
                    .map(|dt| dt.timestamp())
            } else {
                None
            };

            let mut schedules = self.schedules.write().await;
            schedules
                .entry(schedule.container_id.clone())
                .or_default()
                .insert(schedule.id.clone(), schedule);
            restored += 1;
        }

        if restored > 0 {
            info!("Restored {} schedule(s) from disk", restored);
        }
        restored
    }

    /// Validate a cron expression
    pub fn validate_cron(expression: &str) -> Result<()> {
        Schedule::from_str(expression)
            .map_err(|e| NodeError::InvalidInput(format!("Invalid cron expression: {}", e)))?;
        Ok(())
    }

    /// Create a new schedule
    pub async fn create_schedule(
        &self,
        container_id: &str,
        name: &str,
        cron_expression: &str,
        is_active: bool,
        tasks: Vec<ScheduleTask>,
    ) -> Result<ScheduleInfo> {
        // Validate cron expression
        Self::validate_cron(cron_expression)?;

        let schedule_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();

        // Calculate next run time
        let next_run = if is_active {
            Schedule::from_str(cron_expression)
                .ok()
                .and_then(|s| s.upcoming(chrono::Utc).next())
                .map(|dt| dt.timestamp())
        } else {
            None
        };

        let schedule_info = ScheduleInfo {
            id: schedule_id.clone(),
            container_id: container_id.to_string(),
            name: name.to_string(),
            cron_expression: cron_expression.to_string(),
            is_active,
            created_at: now.timestamp(),
            last_run_at: None,
            next_run_at: next_run,
            tasks,
        };

        // Store the schedule
        {
            let mut schedules = self.schedules.write().await;
            let container_schedules =
                schedules.entry(container_id.to_string()).or_insert_with(HashMap::new);
            container_schedules.insert(schedule_id.clone(), schedule_info.clone());
        }

        self.persist(&schedule_info).await;

        info!(
            "Created schedule {} '{}' for container {} ({})",
            schedule_id, name, container_id, cron_expression
        );

        Ok(schedule_info)
    }

    /// List schedules for a container
    pub async fn list_schedules(&self, container_id: &str) -> Result<Vec<ScheduleInfo>> {
        let schedules = self.schedules.read().await;

        if let Some(container_schedules) = schedules.get(container_id) {
            let mut list: Vec<ScheduleInfo> = container_schedules.values().cloned().collect();
            list.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(list)
        } else {
            Ok(Vec::new())
        }
    }

    /// Get a specific schedule
    pub async fn get_schedule(
        &self,
        container_id: &str,
        schedule_id: &str,
    ) -> Result<ScheduleInfo> {
        let schedules = self.schedules.read().await;

        schedules
            .get(container_id)
            .and_then(|cs| cs.get(schedule_id))
            .cloned()
            .ok_or_else(|| {
                NodeError::InvalidInput(format!(
                    "Schedule {} not found for container {}",
                    schedule_id, container_id
                ))
            })
    }

    /// Update a schedule
    pub async fn update_schedule(
        &self,
        container_id: &str,
        schedule_id: &str,
        name: Option<&str>,
        cron_expression: Option<&str>,
        is_active: Option<bool>,
        tasks: Option<Vec<ScheduleTask>>,
    ) -> Result<ScheduleInfo> {
        // Validate cron if provided
        if let Some(expr) = cron_expression {
            Self::validate_cron(expr)?;
        }

        let mut schedules = self.schedules.write().await;

        let container_schedules = schedules.get_mut(container_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("No schedules found for container {}", container_id))
        })?;

        let schedule = container_schedules.get_mut(schedule_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("Schedule {} not found", schedule_id))
        })?;

        // Apply updates
        if let Some(n) = name {
            schedule.name = n.to_string();
        }
        if let Some(expr) = cron_expression {
            schedule.cron_expression = expr.to_string();
        }
        if let Some(active) = is_active {
            schedule.is_active = active;
        }
        if let Some(t) = tasks {
            schedule.tasks = t;
        }

        // Recalculate next run time
        if schedule.is_active {
            schedule.next_run_at = Schedule::from_str(&schedule.cron_expression)
                .ok()
                .and_then(|s| s.upcoming(chrono::Utc).next())
                .map(|dt| dt.timestamp());
        } else {
            schedule.next_run_at = None;
        }

        let updated = schedule.clone();
        drop(schedules);
        self.persist(&updated).await;

        info!(
            "Updated schedule {} for container {}",
            schedule_id, container_id
        );

        Ok(updated)
    }

    /// Delete a schedule
    pub async fn delete_schedule(&self, container_id: &str, schedule_id: &str) -> Result<()> {
        let mut schedules = self.schedules.write().await;

        if let Some(container_schedules) = schedules.get_mut(container_id) {
            if container_schedules.remove(schedule_id).is_some() {
                drop(schedules);
                self.remove_persisted(schedule_id).await;
                info!(
                    "Deleted schedule {} for container {}",
                    schedule_id, container_id
                );
                return Ok(());
            }
        }

        Err(NodeError::InvalidInput(format!(
            "Schedule {} not found for container {}",
            schedule_id, container_id
        )))
    }

    /// Manually trigger a schedule
    pub async fn trigger_schedule(
        &self,
        container_id: &str,
        schedule_id: &str,
        callback: &ScheduleCallback,
    ) -> Result<()> {
        let schedule = self.get_schedule(container_id, schedule_id).await?;

        info!(
            "Manually triggering schedule {} for container {}",
            schedule_id, container_id
        );

        // Execute tasks
        for task in &schedule.tasks {
            if let Err(e) = callback(container_id, task).await {
                warn!("Task execution failed for schedule {}: {}", schedule_id, e);
            }
        }

        // Update last_run_at
        let updated = {
            let mut schedules = self.schedules.write().await;
            schedules
                .get_mut(container_id)
                .and_then(|cs| cs.get_mut(schedule_id))
                .map(|schedule| {
                    schedule.last_run_at = Some(chrono::Utc::now().timestamp());
                    schedule.clone()
                })
        };
        if let Some(updated) = updated {
            self.persist(&updated).await;
        }

        Ok(())
    }

    /// Start the schedule runner background task
    pub async fn start_runner(&self, callback: ScheduleCallback) {
        let schedules = Arc::clone(&self.schedules);
        let shutdown = Arc::clone(&self.shutdown);

        tokio::spawn(async move {
            info!("Schedule runner started");

            loop {
                // Check for shutdown
                if *shutdown.read().await {
                    info!("Schedule runner shutting down");
                    break;
                }

                // Check all schedules
                let now = chrono::Utc::now().timestamp();
                let mut to_run: Vec<(String, String, Vec<ScheduleTask>)> = Vec::new();

                {
                    let schedules = schedules.read().await;
                    for (container_id, container_schedules) in schedules.iter() {
                        for (schedule_id, schedule) in container_schedules.iter() {
                            if !schedule.is_active {
                                continue;
                            }

                            if let Some(next_run) = schedule.next_run_at {
                                if now >= next_run {
                                    to_run.push((
                                        container_id.clone(),
                                        schedule_id.clone(),
                                        schedule.tasks.clone(),
                                    ));
                                }
                            }
                        }
                    }
                }

                // Execute due schedules
                for (container_id, schedule_id, tasks) in to_run {
                    info!(
                        "Executing schedule {} for container {}",
                        schedule_id, container_id
                    );

                    for task in &tasks {
                        // Wait for task offset
                        if task.time_offset > 0 {
                            tokio::time::sleep(tokio::time::Duration::from_secs(
                                task.time_offset as u64,
                            ))
                            .await;
                        }

                        if let Err(e) = callback(&container_id, task).await {
                            warn!("Task execution failed for schedule {}: {}", schedule_id, e);
                        }
                    }

                    // Update schedule
                    {
                        let mut schedules = schedules.write().await;
                        if let Some(container_schedules) = schedules.get_mut(&container_id) {
                            if let Some(schedule) = container_schedules.get_mut(&schedule_id) {
                                schedule.last_run_at = Some(now);

                                // Calculate next run time
                                schedule.next_run_at =
                                    Schedule::from_str(&schedule.cron_expression)
                                        .ok()
                                        .and_then(|s| s.upcoming(chrono::Utc).next())
                                        .map(|dt| dt.timestamp());
                            }
                        }
                    }
                }

                // Sleep until next check
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });
    }

    /// Stop the schedule runner
    pub async fn stop(&self) {
        *self.shutdown.write().await = true;
    }
}

impl Default for ScheduleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_cron() {
        // Valid expressions (cron crate uses 6-part format: sec min hour day month day-of-week)
        assert!(ScheduleManager::validate_cron("0 0 0 * * *").is_ok());
        assert!(ScheduleManager::validate_cron("0 */5 * * * *").is_ok());
        assert!(ScheduleManager::validate_cron("0 0 0 1 * *").is_ok());

        // Invalid expressions
        assert!(ScheduleManager::validate_cron("invalid").is_err());
        assert!(ScheduleManager::validate_cron("60 * * * *").is_err());
    }

    #[tokio::test]
    async fn test_create_and_list_schedule() {
        let manager = ScheduleManager::new();

        let tasks = vec![ScheduleTask {
            task_type: ScheduleTaskType::Command,
            time_offset: 0,
            payload: "save-all".to_string(),
        }];

        // Create schedule (cron crate uses 6-part format: sec min hour day month day-of-week)
        let schedule = manager
            .create_schedule("container-1", "Hourly Save", "0 0 * * * *", true, tasks)
            .await
            .unwrap();

        assert_eq!(schedule.name, "Hourly Save");
        assert!(schedule.is_active);
        assert!(schedule.next_run_at.is_some());

        // List schedules
        let schedules = manager.list_schedules("container-1").await.unwrap();
        assert_eq!(schedules.len(), 1);
    }

    #[tokio::test]
    async fn test_update_schedule() {
        let manager = ScheduleManager::new();

        let schedule = manager
            .create_schedule("container-1", "Test", "0 0 * * * *", true, vec![])
            .await
            .unwrap();

        // Update the schedule
        let updated = manager
            .update_schedule(
                "container-1",
                &schedule.id,
                Some("Updated Name"),
                Some("0 */30 * * * *"),
                Some(false),
                None,
            )
            .await
            .unwrap();

        assert_eq!(updated.name, "Updated Name");
        assert_eq!(updated.cron_expression, "0 */30 * * * *");
        assert!(!updated.is_active);
        assert!(updated.next_run_at.is_none());
    }

    #[tokio::test]
    async fn test_delete_schedule() {
        let manager = ScheduleManager::new();

        let schedule = manager
            .create_schedule("container-1", "Test", "0 0 * * * *", true, vec![])
            .await
            .unwrap();

        // Delete the schedule
        manager.delete_schedule("container-1", &schedule.id).await.unwrap();

        // Verify it's gone
        let schedules = manager.list_schedules("container-1").await.unwrap();
        assert!(schedules.is_empty());
    }

    #[tokio::test]
    async fn test_schedule_persists_and_restores() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();

        // Manager A creates a schedule, then goes away.
        {
            let manager = ScheduleManager::with_data_dir(dir.clone());
            manager
                .create_schedule(
                    "container-1",
                    "Nightly Backup",
                    "0 0 4 * * *",
                    true,
                    vec![ScheduleTask {
                        task_type: ScheduleTaskType::Backup,
                        time_offset: 0,
                        payload: String::new(),
                    }],
                )
                .await
                .unwrap();
        }

        // A fresh manager restores from disk.
        let manager2 = ScheduleManager::with_data_dir(dir);
        assert!(manager2.list_schedules("container-1").await.unwrap().is_empty());

        assert_eq!(manager2.restore().await, 1);

        let schedules = manager2.list_schedules("container-1").await.unwrap();
        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].name, "Nightly Backup");
        // next_run_at was recomputed from the cron expression on restore.
        assert!(schedules[0].next_run_at.is_some());
    }

    #[tokio::test]
    async fn test_delete_removes_persisted_schedule() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();
        let manager = ScheduleManager::with_data_dir(dir.clone());

        let schedule = manager
            .create_schedule("container-1", "Test", "0 0 * * * *", true, vec![])
            .await
            .unwrap();
        manager.delete_schedule("container-1", &schedule.id).await.unwrap();

        // A fresh manager finds nothing to restore.
        let manager2 = ScheduleManager::with_data_dir(dir);
        assert_eq!(manager2.restore().await, 0);
    }

    #[tokio::test]
    async fn test_dispatch_callback_rejects_unknown_power_action() {
        use crate::backup::BackupManager;
        use crate::container::ContainerManager;

        let temp = tempfile::TempDir::new().unwrap();
        let manager = Arc::new(ContainerManager::new(temp.path().to_path_buf()));
        let backup = Arc::new(BackupManager::new(temp.path()));
        let cb = dispatch_callback(manager, backup);

        // An unrecognized power action is reported as an error rather than
        // silently succeeding (the old no-op behavior).
        let task = ScheduleTask {
            task_type: ScheduleTaskType::Power,
            time_offset: 0,
            payload: "frobnicate".to_string(),
        };
        let result = cb("container-1", &task).await;
        assert!(result.is_err());
    }
}

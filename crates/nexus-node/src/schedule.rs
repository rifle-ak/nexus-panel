//! Schedule management for automated container tasks
//!
//! Provides cron-based scheduling for container operations:
//! - Console commands
//! - Power actions (start, stop, restart)
//! - Backup creation

use crate::error::{NodeError, Result};
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Schedule task type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
pub struct ScheduleTask {
    pub task_type: ScheduleTaskType,
    pub time_offset: u32,  // Seconds after schedule triggers
    pub payload: String,    // Task-specific payload
}

/// Schedule information
#[derive(Debug, Clone)]
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
pub type ScheduleCallback = Arc<dyn Fn(&str, &ScheduleTask) -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync>;

/// Schedule manager
pub struct ScheduleManager {
    /// Schedules by container_id -> schedule_id -> ScheduleInfo
    schedules: Arc<RwLock<HashMap<String, HashMap<String, ScheduleInfo>>>>,

    /// Shutdown signal
    shutdown: Arc<RwLock<bool>>,
}

impl ScheduleManager {
    /// Create a new schedule manager
    pub fn new() -> Self {
        Self {
            schedules: Arc::new(RwLock::new(HashMap::new())),
            shutdown: Arc::new(RwLock::new(false)),
        }
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
            let container_schedules = schedules
                .entry(container_id.to_string())
                .or_insert_with(HashMap::new);
            container_schedules.insert(schedule_id.clone(), schedule_info.clone());
        }

        info!(
            "Created schedule {} '{}' for container {} ({})",
            schedule_id,
            name,
            container_id,
            cron_expression
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
    pub async fn get_schedule(&self, container_id: &str, schedule_id: &str) -> Result<ScheduleInfo> {
        let schedules = self.schedules.read().await;

        schedules
            .get(container_id)
            .and_then(|cs| cs.get(schedule_id))
            .cloned()
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "Schedule {} not found for container {}",
                schedule_id,
                container_id
            )))
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

        let container_schedules = schedules
            .get_mut(container_id)
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "No schedules found for container {}",
                container_id
            )))?;

        let schedule = container_schedules
            .get_mut(schedule_id)
            .ok_or_else(|| NodeError::InvalidInput(format!(
                "Schedule {} not found",
                schedule_id
            )))?;

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

        info!(
            "Updated schedule {} for container {}",
            schedule_id,
            container_id
        );

        Ok(schedule.clone())
    }

    /// Delete a schedule
    pub async fn delete_schedule(&self, container_id: &str, schedule_id: &str) -> Result<()> {
        let mut schedules = self.schedules.write().await;

        if let Some(container_schedules) = schedules.get_mut(container_id) {
            if container_schedules.remove(schedule_id).is_some() {
                info!(
                    "Deleted schedule {} for container {}",
                    schedule_id,
                    container_id
                );
                return Ok(());
            }
        }

        Err(NodeError::InvalidInput(format!(
            "Schedule {} not found for container {}",
            schedule_id,
            container_id
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
            schedule_id,
            container_id
        );

        // Execute tasks
        for task in &schedule.tasks {
            if let Err(e) = callback(container_id, task).await {
                warn!(
                    "Task execution failed for schedule {}: {}",
                    schedule_id,
                    e
                );
            }
        }

        // Update last_run_at
        {
            let mut schedules = self.schedules.write().await;
            if let Some(container_schedules) = schedules.get_mut(container_id) {
                if let Some(schedule) = container_schedules.get_mut(schedule_id) {
                    schedule.last_run_at = Some(chrono::Utc::now().timestamp());
                }
            }
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
                        schedule_id,
                        container_id
                    );

                    for task in &tasks {
                        // Wait for task offset
                        if task.time_offset > 0 {
                            tokio::time::sleep(tokio::time::Duration::from_secs(task.time_offset as u64)).await;
                        }

                        if let Err(e) = callback(&container_id, task).await {
                            warn!(
                                "Task execution failed for schedule {}: {}",
                                schedule_id,
                                e
                            );
                        }
                    }

                    // Update schedule
                    {
                        let mut schedules = schedules.write().await;
                        if let Some(container_schedules) = schedules.get_mut(&container_id) {
                            if let Some(schedule) = container_schedules.get_mut(&schedule_id) {
                                schedule.last_run_at = Some(now);

                                // Calculate next run time
                                schedule.next_run_at = Schedule::from_str(&schedule.cron_expression)
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

        let tasks = vec![
            ScheduleTask {
                task_type: ScheduleTaskType::Command,
                time_offset: 0,
                payload: "save-all".to_string(),
            },
        ];

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
}

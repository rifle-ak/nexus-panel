//! Scheduled tasks: console commands, power actions and backups on a cron.
//!
//! Expressions are the five-field cron everyone writes (`0 4 * * *`); the
//! six- and seven-field forms with seconds and years are accepted too.
//! Each schedule may carry an IANA time zone, so "4 a.m." means the
//! operator's 4 a.m.; without one it is UTC.
//!
//! The runner ticks every second. A schedule that is due is marked running,
//! has its next run computed at once, and executes on its own task, so a
//! task's `time_offset` delays that schedule alone and a slow backup never
//! holds up anything else. A schedule still running when it comes due again
//! is skipped for that run, not stacked.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::TimeZone;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use crate::backup::BackupManager;
use crate::container::ContainerManager;
use crate::error::{NodeError, Result};

/// Subdirectory of `DATA_DIR` where schedules are persisted.
const SCHEDULE_SUBDIR: &str = ".nexus/schedules";

/// How long a scheduled backup gives the game after its pre-backup command
/// (`save-all`) before archiving.
pub const PRE_BACKUP_SETTLE: Duration = Duration::from_secs(5);

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
    /// IANA zone the expression is read in; `None` is UTC.
    #[serde(default)]
    pub timezone: Option<String>,
    pub is_active: bool,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    /// What went wrong on the last run, if anything did.
    #[serde(default)]
    pub last_error: Option<String>,
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
            let result = run_task(&manager, &backup_manager, &container_id, &task).await;
            if let Err(e) = &result {
                let name = manager
                    .get_state(&container_id)
                    .await
                    .map(|s| s.name)
                    .unwrap_or_else(|_| container_id.clone());
                manager.notify(
                    crate::notify::Notification::new(
                        "schedule.failed",
                        crate::notify::Severity::Warning,
                        format!("Scheduled task failed on {}", name),
                        format!("{:?} {:?}: {}", task.task_type, task.payload, e),
                    )
                    .for_server(&container_id, &name),
                );
            }
            result
        })
    })
}

async fn run_task(
    manager: &ContainerManager,
    backup_manager: &BackupManager,
    container_id: &str,
    task: &ScheduleTask,
) -> Result<()> {
    {
        {
            match task.task_type {
                ScheduleTaskType::Command => {
                    manager.send_command(container_id, &task.payload).await
                }
                ScheduleTaskType::Power => match task.payload.trim().to_lowercase().as_str() {
                    "start" => manager.start_container(container_id).await,
                    "stop" => manager.stop_container(container_id, None).await,
                    "restart" => manager.restart_container(container_id).await,
                    other => Err(NodeError::InvalidInput(format!(
                        "Unknown power action: {}",
                        other
                    ))),
                },
                ScheduleTaskType::Backup => {
                    let name = if task.payload.is_empty() {
                        format!("scheduled-{}", chrono::Utc::now().format("%Y%m%d-%H%M"))
                    } else {
                        task.payload.clone()
                    };
                    crate::backup::backup_server(
                        manager,
                        backup_manager,
                        container_id,
                        &name,
                        None,
                        PRE_BACKUP_SETTLE,
                    )
                    .await
                    .map(|_| ())
                }
            }
        }
    }
}

/// Parse a cron expression in five-, six- or seven-field form.
pub fn parse_cron(expression: &str) -> Result<Schedule> {
    let fields = expression.split_whitespace().count();
    let full = match fields {
        5 => format!("0 {}", expression.trim()),
        6 | 7 => expression.trim().to_string(),
        _ => {
            return Err(NodeError::InvalidInput(format!(
                "Invalid cron expression {:?}: expected 5 fields (minute hour day month weekday)",
                expression
            )))
        }
    };
    Schedule::from_str(&full)
        .map_err(|e| NodeError::InvalidInput(format!("Invalid cron expression: {}", e)))
}

/// Resolve a zone name; `None` and empty mean UTC.
pub fn parse_timezone(name: Option<&str>) -> Result<chrono_tz::Tz> {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        None => Ok(chrono_tz::UTC),
        Some(n) => chrono_tz::Tz::from_str(n)
            .map_err(|_| NodeError::InvalidInput(format!("Unknown time zone {:?}", n))),
    }
}

/// The next time an expression fires after `after`, in the zone, as a Unix
/// timestamp.
pub fn next_run_after(
    expression: &str,
    timezone: Option<&str>,
    after: chrono::DateTime<chrono::Utc>,
) -> Result<Option<i64>> {
    let schedule = parse_cron(expression)?;
    let tz = parse_timezone(timezone)?;
    let after_local = tz.from_utc_datetime(&after.naive_utc());
    Ok(schedule.after(&after_local).next().map(|dt| dt.timestamp()))
}

fn next_run(expression: &str, timezone: Option<&str>) -> Option<i64> {
    next_run_after(expression, timezone, chrono::Utc::now()).ok().flatten()
}

/// Schedule manager
pub struct ScheduleManager {
    /// Schedules per container
    schedules: Arc<RwLock<HashMap<String, HashMap<String, ScheduleInfo>>>>,

    /// Ids of schedules executing right now.
    running: Arc<RwLock<HashSet<String>>>,

    /// Runner shutdown signal
    shutdown: Arc<RwLock<bool>>,

    /// Where schedules are persisted (None = in-memory only, for tests).
    data_dir: Option<PathBuf>,
}

impl ScheduleManager {
    /// Create a new schedule manager (in-memory only; nothing survives a restart).
    pub fn new() -> Self {
        Self {
            schedules: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(RwLock::new(HashSet::new())),
            shutdown: Arc::new(RwLock::new(false)),
            data_dir: None,
        }
    }

    /// Create a schedule manager that persists schedules under `data_dir`.
    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            schedules: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(RwLock::new(HashSet::new())),
            shutdown: Arc::new(RwLock::new(false)),
            data_dir: Some(data_dir),
        }
    }

    fn schedule_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join(SCHEDULE_SUBDIR))
    }

    /// Write a schedule to disk (best-effort; failures are logged, not fatal).
    async fn persist(&self, schedule: &ScheduleInfo) {
        persist_to(self.schedule_dir(), schedule).await;
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
                next_run(&schedule.cron_expression, schedule.timezone.as_deref())
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
        parse_cron(expression).map(|_| ())
    }

    /// Create a new schedule
    pub async fn create_schedule(
        &self,
        container_id: &str,
        name: &str,
        cron_expression: &str,
        timezone: Option<&str>,
        is_active: bool,
        tasks: Vec<ScheduleTask>,
    ) -> Result<ScheduleInfo> {
        Self::validate_cron(cron_expression)?;
        let tz = parse_timezone(timezone)?;
        let timezone = (tz != chrono_tz::UTC).then(|| tz.name().to_string());

        let schedule_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();

        let next_run_at = if is_active {
            next_run(cron_expression, timezone.as_deref())
        } else {
            None
        };

        let schedule_info = ScheduleInfo {
            id: schedule_id.clone(),
            container_id: container_id.to_string(),
            name: name.to_string(),
            cron_expression: cron_expression.trim().to_string(),
            timezone,
            is_active,
            created_at: now.timestamp(),
            last_run_at: None,
            next_run_at,
            last_error: None,
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

    /// Update a schedule. `timezone` of `Some("")` or `Some("UTC")` clears
    /// the zone; `None` leaves it as it is.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_schedule(
        &self,
        container_id: &str,
        schedule_id: &str,
        name: Option<&str>,
        cron_expression: Option<&str>,
        timezone: Option<&str>,
        is_active: Option<bool>,
        tasks: Option<Vec<ScheduleTask>>,
    ) -> Result<ScheduleInfo> {
        if let Some(expr) = cron_expression {
            Self::validate_cron(expr)?;
        }
        let new_timezone = match timezone {
            Some(tz) => {
                let tz = parse_timezone(Some(tz))?;
                Some((tz != chrono_tz::UTC).then(|| tz.name().to_string()))
            }
            None => None,
        };

        let mut schedules = self.schedules.write().await;

        let container_schedules = schedules.get_mut(container_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("No schedules found for container {}", container_id))
        })?;

        let schedule = container_schedules.get_mut(schedule_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("Schedule {} not found", schedule_id))
        })?;

        if let Some(n) = name {
            schedule.name = n.to_string();
        }
        if let Some(expr) = cron_expression {
            schedule.cron_expression = expr.trim().to_string();
        }
        if let Some(tz) = new_timezone {
            schedule.timezone = tz;
        }
        if let Some(active) = is_active {
            schedule.is_active = active;
        }
        if let Some(t) = tasks {
            schedule.tasks = t;
        }

        schedule.next_run_at = if schedule.is_active {
            next_run(&schedule.cron_expression, schedule.timezone.as_deref())
        } else {
            None
        };

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

    /// Delete every schedule of a container (it is being removed).
    pub async fn delete_all(&self, container_id: &str) {
        let removed = self.schedules.write().await.remove(container_id);
        if let Some(removed) = removed {
            for id in removed.keys() {
                self.remove_persisted(id).await;
            }
        }
    }

    /// Run a schedule now, on its own task. Returns at once; `Err` when the
    /// schedule does not exist or is already running.
    pub async fn trigger_schedule(
        &self,
        container_id: &str,
        schedule_id: &str,
        callback: &ScheduleCallback,
    ) -> Result<()> {
        let schedule = self.get_schedule(container_id, schedule_id).await?;
        if !self.running.write().await.insert(schedule.id.clone()) {
            return Err(NodeError::InvalidInput(format!(
                "Schedule {} is already running",
                schedule_id
            )));
        }
        info!(
            "Manually triggering schedule {} for container {}",
            schedule_id, container_id
        );
        self.spawn_run(schedule, callback.clone());
        Ok(())
    }

    /// Whether a schedule is executing right now.
    pub async fn is_running(&self, schedule_id: &str) -> bool {
        self.running.read().await.contains(schedule_id)
    }

    /// Execute a schedule's tasks on a new task; the caller has already put
    /// it in `running`.
    fn spawn_run(&self, schedule: ScheduleInfo, callback: ScheduleCallback) {
        let schedules = Arc::clone(&self.schedules);
        let running = Arc::clone(&self.running);
        let dir = self.schedule_dir();
        tokio::spawn(async move {
            let started = chrono::Utc::now().timestamp();
            let error = run_tasks(&schedule, &callback).await;
            let updated = {
                let mut all = schedules.write().await;
                all.get_mut(&schedule.container_id).and_then(|cs| cs.get_mut(&schedule.id)).map(
                    |s| {
                        s.last_run_at = Some(started);
                        s.last_error = error;
                        s.clone()
                    },
                )
            };
            if let Some(updated) = updated {
                persist_to(dir, &updated).await;
            }
            running.write().await.remove(&schedule.id);
        });
    }

    /// Start the schedule runner background task
    pub async fn start_runner(&self, callback: ScheduleCallback) {
        let this = self.clone_handles();
        tokio::spawn(async move {
            info!("Schedule runner started");
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if *this.shutdown.read().await {
                    info!("Schedule runner shutting down");
                    break;
                }
                this.dispatch_due(&callback).await;
            }
        });
    }

    /// Start every schedule that is due and not already running. Public so
    /// tests can tick the runner by hand.
    pub async fn dispatch_due(&self, callback: &ScheduleCallback) {
        let now = chrono::Utc::now().timestamp();
        let mut due: Vec<ScheduleInfo> = Vec::new();
        {
            let running = self.running.read().await;
            let mut schedules = self.schedules.write().await;
            for container_schedules in schedules.values_mut() {
                for schedule in container_schedules.values_mut() {
                    if !schedule.is_active || running.contains(&schedule.id) {
                        continue;
                    }
                    let Some(next) = schedule.next_run_at else {
                        continue;
                    };
                    if now < next {
                        continue;
                    }
                    // The next run is fixed now, not when this one finishes,
                    // so a long task does not shift the timetable.
                    schedule.next_run_at =
                        next_run(&schedule.cron_expression, schedule.timezone.as_deref());
                    due.push(schedule.clone());
                }
            }
        }
        for schedule in due {
            info!(
                "Executing schedule {} '{}' for container {}",
                schedule.id, schedule.name, schedule.container_id
            );
            self.running.write().await.insert(schedule.id.clone());
            self.persist(&schedule).await;
            self.spawn_run(schedule, callback.clone());
        }
    }

    fn clone_handles(&self) -> Self {
        Self {
            schedules: Arc::clone(&self.schedules),
            running: Arc::clone(&self.running),
            shutdown: Arc::clone(&self.shutdown),
            data_dir: self.data_dir.clone(),
        }
    }

    /// Stop the schedule runner
    pub async fn stop(&self) {
        *self.shutdown.write().await = true;
    }
}

/// Run a schedule's tasks in order, honouring each one's offset. Returns
/// the first failure's message; the remaining tasks still run.
async fn run_tasks(schedule: &ScheduleInfo, callback: &ScheduleCallback) -> Option<String> {
    let mut error = None;
    for task in &schedule.tasks {
        if task.time_offset > 0 {
            tokio::time::sleep(Duration::from_secs(task.time_offset as u64)).await;
        }
        if let Err(e) = callback(&schedule.container_id, task).await {
            warn!(
                "Task {:?} of schedule {} failed: {}",
                task.task_type, schedule.id, e
            );
            error.get_or_insert_with(|| format!("{:?}: {}", task.task_type, e));
        }
    }
    error
}

async fn persist_to(dir: Option<PathBuf>, schedule: &ScheduleInfo) {
    let Some(dir) = dir else {
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
        // Five fields, as people write them.
        assert!(ScheduleManager::validate_cron("0 4 * * *").is_ok());
        assert!(ScheduleManager::validate_cron("*/5 * * * *").is_ok());
        // Six and seven fields still work.
        assert!(ScheduleManager::validate_cron("0 0 0 * * *").is_ok());
        assert!(ScheduleManager::validate_cron("0 */5 * * * *").is_ok());
        assert!(ScheduleManager::validate_cron("0 0 0 1 * * 2030").is_ok());

        assert!(ScheduleManager::validate_cron("invalid").is_err());
        assert!(ScheduleManager::validate_cron("60 * * * *").is_err());
        assert!(ScheduleManager::validate_cron("* * *").is_err());
    }

    #[test]
    fn next_run_respects_the_time_zone() {
        // 2030-06-01 00:00 UTC. "4 a.m." in New York (UTC-4 in June) is
        // 08:00 UTC; in UTC it is 04:00.
        let after = chrono::Utc.with_ymd_and_hms(2030, 6, 1, 0, 0, 0).unwrap();
        let utc = next_run_after("0 4 * * *", None, after).unwrap().unwrap();
        let ny = next_run_after("0 4 * * *", Some("America/New_York"), after).unwrap().unwrap();
        assert_eq!(utc, after.timestamp() + 4 * 3600);
        assert_eq!(ny, after.timestamp() + 8 * 3600);
        assert!(parse_timezone(Some("Mars/Olympus")).is_err());
        assert_eq!(parse_timezone(Some("")).unwrap(), chrono_tz::UTC);
    }

    #[tokio::test]
    async fn test_create_and_list_schedule() {
        let manager = ScheduleManager::new();

        let tasks = vec![ScheduleTask {
            task_type: ScheduleTaskType::Command,
            time_offset: 0,
            payload: "save-all".to_string(),
        }];

        let schedule = manager
            .create_schedule(
                "container-1",
                "Hourly Save",
                "0 * * * *",
                Some("Europe/Berlin"),
                true,
                tasks,
            )
            .await
            .unwrap();

        assert_eq!(schedule.name, "Hourly Save");
        assert_eq!(schedule.timezone.as_deref(), Some("Europe/Berlin"));
        assert!(schedule.is_active);
        assert!(schedule.next_run_at.is_some());

        // A UTC zone is stored as "no zone".
        let plain = manager
            .create_schedule(
                "container-1",
                "Plain",
                "0 * * * *",
                Some("UTC"),
                true,
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(plain.timezone, None);

        // List schedules
        let schedules = manager.list_schedules("container-1").await.unwrap();
        assert_eq!(schedules.len(), 2);
    }

    #[tokio::test]
    async fn test_update_schedule() {
        let manager = ScheduleManager::new();

        let schedule = manager
            .create_schedule("container-1", "Test", "0 0 * * * *", None, true, vec![])
            .await
            .unwrap();

        let updated = manager
            .update_schedule(
                "container-1",
                &schedule.id,
                Some("Updated Name"),
                Some("*/30 * * * *"),
                Some("Asia/Tokyo"),
                Some(false),
                None,
            )
            .await
            .unwrap();

        assert_eq!(updated.name, "Updated Name");
        assert_eq!(updated.cron_expression, "*/30 * * * *");
        assert_eq!(updated.timezone.as_deref(), Some("Asia/Tokyo"));
        assert!(!updated.is_active);
        assert!(updated.next_run_at.is_none());

        // Clearing the zone and re-enabling.
        let updated = manager
            .update_schedule(
                "container-1",
                &schedule.id,
                None,
                None,
                Some(""),
                Some(true),
                None,
            )
            .await
            .unwrap();
        assert_eq!(updated.timezone, None);
        assert!(updated.next_run_at.is_some());
    }

    #[tokio::test]
    async fn test_delete_schedule() {
        let manager = ScheduleManager::new();

        let schedule = manager
            .create_schedule("container-1", "Test", "0 0 * * * *", None, true, vec![])
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
                    "0 4 * * *",
                    Some("America/Chicago"),
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
        assert_eq!(schedules[0].timezone.as_deref(), Some("America/Chicago"));
        // next_run_at was recomputed from the cron expression on restore.
        assert!(schedules[0].next_run_at.is_some());
    }

    #[tokio::test]
    async fn schedules_written_by_older_builds_still_load() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().join(SCHEDULE_SUBDIR);
        std::fs::create_dir_all(&dir).unwrap();
        // No timezone or last_error fields, six-field cron.
        std::fs::write(
            dir.join("old.json"),
            r#"{"id":"old","container_id":"c","name":"Old","cron_expression":"0 0 4 * * *","is_active":true,"created_at":1,"last_run_at":null,"next_run_at":null,"tasks":[]}"#,
        )
        .unwrap();
        let manager = ScheduleManager::with_data_dir(temp.path().to_path_buf());
        assert_eq!(manager.restore().await, 1);
        let s = manager.get_schedule("c", "old").await.unwrap();
        assert_eq!(s.timezone, None);
        assert!(s.next_run_at.is_some());
    }

    #[tokio::test]
    async fn test_delete_removes_persisted_schedule() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();
        let manager = ScheduleManager::with_data_dir(dir.clone());

        let schedule = manager
            .create_schedule("container-1", "Test", "0 0 * * * *", None, true, vec![])
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

    /// A callback that records what ran and can be told to fail or stall.
    fn recording_callback(
        log: Arc<RwLock<Vec<String>>>,
        fail_on: Option<&'static str>,
    ) -> ScheduleCallback {
        Arc::new(move |container_id: &str, task: &ScheduleTask| {
            let log = log.clone();
            let entry = format!("{}:{}", container_id, task.payload);
            let payload = task.payload.clone();
            Box::pin(async move {
                if payload == "slow" {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                log.write().await.push(entry);
                if Some(payload.as_str()) == fail_on {
                    Err(NodeError::Internal("boom".into()))
                } else {
                    Ok(())
                }
            })
        })
    }

    async fn wait_until_idle(manager: &ScheduleManager, id: &str) {
        for _ in 0..100 {
            if !manager.is_running(id).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("schedule {} did not finish", id);
    }

    #[tokio::test]
    async fn due_schedules_run_concurrently_and_record_errors() {
        let manager = ScheduleManager::new();
        let log = Arc::new(RwLock::new(Vec::new()));
        let cb = recording_callback(log.clone(), Some("bad"));

        let slow = manager
            .create_schedule(
                "c1",
                "Slow",
                "* * * * *",
                None,
                true,
                vec![ScheduleTask {
                    task_type: ScheduleTaskType::Command,
                    time_offset: 0,
                    payload: "slow".into(),
                }],
            )
            .await
            .unwrap();
        let quick = manager
            .create_schedule(
                "c2",
                "Quick",
                "* * * * *",
                None,
                true,
                vec![
                    ScheduleTask {
                        task_type: ScheduleTaskType::Command,
                        time_offset: 0,
                        payload: "bad".into(),
                    },
                    ScheduleTask {
                        task_type: ScheduleTaskType::Command,
                        time_offset: 0,
                        payload: "after".into(),
                    },
                ],
            )
            .await
            .unwrap();
        // Make both due now.
        {
            let mut all = manager.schedules.write().await;
            for cs in all.values_mut() {
                for s in cs.values_mut() {
                    s.next_run_at = Some(0);
                }
            }
        }

        manager.dispatch_due(&cb).await;
        // Both are running; the quick one finishes while the slow one is
        // still asleep, so the slow schedule did not block it.
        assert!(manager.is_running(&slow.id).await);
        wait_until_idle(&manager, &quick.id).await;
        assert!(manager.is_running(&slow.id).await);
        // A second tick does not start the slow one again while it runs.
        manager.dispatch_due(&cb).await;
        wait_until_idle(&manager, &slow.id).await;
        assert_eq!(
            log.read().await.iter().filter(|e| e.ends_with(":slow")).count(),
            1
        );

        let q = manager.get_schedule("c2", &quick.id).await.unwrap();
        assert!(q.last_run_at.is_some());
        assert!(q.last_error.as_deref().unwrap().contains("boom"));
        // The task after the failing one still ran.
        assert!(log.read().await.contains(&"c2:after".to_string()));
        let s = manager.get_schedule("c1", &slow.id).await.unwrap();
        assert_eq!(s.last_error, None);
        // Next run was moved into the future when dispatched.
        assert!(s.next_run_at.unwrap() > chrono::Utc::now().timestamp() - 1);

        // Manual trigger refuses to stack on a running schedule.
        manager.trigger_schedule("c1", &slow.id, &cb).await.unwrap();
        assert!(manager.trigger_schedule("c1", &slow.id, &cb).await.is_err());
        wait_until_idle(&manager, &slow.id).await;
    }
}

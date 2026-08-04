//! Per-server game-file update executor.
//!
//! Turns a server's configured [`UpdateApply`] strategy into a concrete
//! command and runs it inside the container as a background job. This is what
//! makes the blueprint `updates` block *actionable* — a "Update / Reinstall"
//! button that actually re-runs SteamCMD or DepotDownloader against a running
//! server, rather than the strategy being purely declarative.
//!
//! Updates can take many minutes (multi-GB downloads), so they run detached in
//! a spawned task with a long timeout; callers start a job and then poll its
//! status.

use nexus_config::UpdateApply;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;

/// How long a background update may run before we give up. Game downloads are
/// slow, so this is generous compared with the interactive shell exec.
pub const UPDATE_TIMEOUT: Duration = Duration::from_secs(3600);

/// Default in-container directory to install/update into. Matches the
/// `working_dir` used by the shipped blueprints.
pub const DEFAULT_INSTALL_DIR: &str = "/home/container";

/// Quote a string for safe inclusion in a `/bin/sh -c` command line.
fn shell_quote(s: &str) -> String {
    // Wrap in single quotes and escape any embedded single quote.
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build the shell command that applies `apply` into `install_dir`.
///
/// Returns `Err` for strategies that cannot be executed as an in-container
/// command (currently the Docker image-pull strategy, which is a runtime/host
/// concern rather than something to run inside the game container).
///
/// The commands assume the relevant tool (`steamcmd`, `DepotDownloader`,
/// `curl`) is available on `PATH` inside the container image — the same
/// assumption the blueprints' lifecycle commands already make.
pub fn build_update_command(apply: &UpdateApply, install_dir: &str) -> Result<String, String> {
    let dir = shell_quote(install_dir);
    match apply {
        UpdateApply::SteamCmd { app_id, beta } => {
            let mut cmd =
                format!("steamcmd +force_install_dir {dir} +login anonymous +app_update {app_id}");
            if let Some(beta) = beta.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                cmd.push_str(&format!(" -beta {}", shell_quote(beta)));
            }
            cmd.push_str(" validate +quit");
            Ok(cmd)
        }
        UpdateApply::DepotDownloader {
            app_id,
            depot_id,
            manifest,
            branch,
        } => {
            let mut cmd = format!("DepotDownloader -app {app_id}");
            if let Some(depot) = depot_id {
                cmd.push_str(&format!(" -depot {depot}"));
            }
            if let Some(manifest) = manifest.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                cmd.push_str(&format!(" -manifest {}", shell_quote(manifest)));
            }
            if let Some(branch) = branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                cmd.push_str(&format!(" -branch {}", shell_quote(branch)));
            }
            cmd.push_str(&format!(" -dir {dir}"));
            Ok(cmd)
        }
        UpdateApply::Download { url } => {
            // Fetch a single artifact into the install directory, keeping the
            // remote file name.
            Ok(format!(
                "mkdir -p {dir} && cd {dir} && curl -fSLO {}",
                shell_quote(url)
            ))
        }
        UpdateApply::Command { command } => {
            // Run the operator-provided command from the install directory.
            Ok(format!("cd {dir} && {command}"))
        }
        UpdateApply::Docker { .. } => Err(
            "the Docker update strategy pulls a new image at the runtime/host level and \
             cannot be applied by running a command inside the container"
                .to_string(),
        ),
    }
}

/// Lifecycle status of a background update job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    Running,
    Succeeded,
    Failed,
}

/// A snapshot of an update job, safe to serialize to the API.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateJob {
    /// Unique job id.
    pub id: String,
    /// Container this update targets.
    pub container_id: String,
    pub status: UpdateStatus,
    /// The shell command that was run.
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    /// Process exit code (once finished).
    pub exit_code: Option<i32>,
    /// Error message if the job failed to run (distinct from a non-zero exit).
    pub error: Option<String>,
    /// Unix seconds when the job started.
    pub started_at: u64,
    /// Unix seconds when the job finished (once finished).
    pub finished_at: Option<u64>,
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl UpdateJob {
    fn new(id: String, container_id: String, command: String) -> Self {
        Self {
            id,
            container_id,
            status: UpdateStatus::Running,
            command,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            error: None,
            started_at: unix_secs(SystemTime::now()),
            finished_at: None,
        }
    }
}

/// In-memory registry of update jobs, one active job per container.
#[derive(Default)]
pub struct UpdateJobStore {
    jobs: RwLock<HashMap<String, UpdateJob>>,
}

impl UpdateJobStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new running job for `container_id`, returning its snapshot.
    /// Fails if an update is already running for that container.
    pub async fn start(&self, container_id: &str, command: String) -> Result<UpdateJob, String> {
        let mut jobs = self.jobs.write().await;
        if let Some(existing) = jobs.get(container_id) {
            if existing.status == UpdateStatus::Running {
                return Err("an update is already running for this server".to_string());
            }
        }
        let id = format!("upd-{}", uuid::Uuid::new_v4());
        let job = UpdateJob::new(id, container_id.to_string(), command);
        jobs.insert(container_id.to_string(), job.clone());
        Ok(job)
    }

    /// Get the current job snapshot for a container, if any.
    pub async fn get(&self, container_id: &str) -> Option<UpdateJob> {
        self.jobs.read().await.get(container_id).cloned()
    }

    /// Mark a job finished with captured output.
    pub async fn finish(
        &self,
        container_id: &str,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    ) {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(container_id) {
            job.stdout = stdout;
            job.stderr = stderr;
            job.exit_code = exit_code;
            job.status = if exit_code == Some(0) {
                UpdateStatus::Succeeded
            } else {
                UpdateStatus::Failed
            };
            job.finished_at = Some(unix_secs(SystemTime::now()));
        }
    }

    /// Mark a job failed to run at all (e.g. the exec call errored).
    pub async fn fail(&self, container_id: &str, error: String) {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(container_id) {
            job.status = UpdateStatus::Failed;
            job.error = Some(error);
            job.finished_at = Some(unix_secs(SystemTime::now()));
        }
    }
}

/// Shared handle used by the web layer.
pub type SharedUpdateJobStore = Arc<UpdateJobStore>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steamcmd_command_includes_appid_and_validate() {
        let apply = UpdateApply::SteamCmd {
            app_id: 258550,
            beta: None,
        };
        let cmd = build_update_command(&apply, "/home/container").unwrap();
        assert_eq!(
            cmd,
            "steamcmd +force_install_dir '/home/container' +login anonymous \
             +app_update 258550 validate +quit"
        );
    }

    #[test]
    fn steamcmd_command_includes_beta_when_set() {
        let apply = UpdateApply::SteamCmd {
            app_id: 258550,
            beta: Some("staging".to_string()),
        };
        let cmd = build_update_command(&apply, "/home/container").unwrap();
        assert!(cmd.contains("-beta 'staging'"));
        assert!(cmd.trim_end().ends_with("validate +quit"));
    }

    #[test]
    fn depot_downloader_command_builds_flags() {
        let apply = UpdateApply::DepotDownloader {
            app_id: 258550,
            depot_id: Some(258551),
            manifest: Some("123456".to_string()),
            branch: Some("public".to_string()),
        };
        let cmd = build_update_command(&apply, "/home/container").unwrap();
        assert_eq!(
            cmd,
            "DepotDownloader -app 258550 -depot 258551 -manifest '123456' \
             -branch 'public' -dir '/home/container'"
        );
    }

    #[test]
    fn depot_downloader_minimal() {
        let apply = UpdateApply::DepotDownloader {
            app_id: 258550,
            depot_id: None,
            manifest: None,
            branch: None,
        };
        let cmd = build_update_command(&apply, "/home/container").unwrap();
        assert_eq!(cmd, "DepotDownloader -app 258550 -dir '/home/container'");
    }

    #[test]
    fn docker_strategy_is_rejected() {
        let apply = UpdateApply::Docker { pull: true };
        assert!(build_update_command(&apply, "/home/container").is_err());
    }

    #[test]
    fn install_dir_is_shell_quoted() {
        // A malicious/awkward path can't break out of the quoting.
        let apply = UpdateApply::SteamCmd {
            app_id: 1,
            beta: None,
        };
        let cmd = build_update_command(&apply, "/home/co'; rm -rf /").unwrap();
        assert!(cmd.contains("'/home/co'\\''; rm -rf /'"));
        assert!(!cmd.contains("+force_install_dir /home/co; rm"));
    }

    #[tokio::test]
    async fn store_rejects_concurrent_job_then_allows_after_finish() {
        let store = UpdateJobStore::new();
        let job = store.start("c1", "steamcmd ...".to_string()).await.unwrap();
        assert_eq!(job.status, UpdateStatus::Running);

        // A second start while running is rejected.
        assert!(store.start("c1", "x".to_string()).await.is_err());

        // After finishing, a new job can start.
        store.finish("c1", "ok".to_string(), String::new(), Some(0)).await;
        let got = store.get("c1").await.unwrap();
        assert_eq!(got.status, UpdateStatus::Succeeded);
        assert!(got.finished_at.is_some());
        assert!(store.start("c1", "y".to_string()).await.is_ok());
    }

    #[tokio::test]
    async fn store_marks_failure() {
        let store = UpdateJobStore::new();
        store.start("c2", "cmd".to_string()).await.unwrap();
        store.finish("c2", String::new(), "boom".to_string(), Some(1)).await;
        assert_eq!(store.get("c2").await.unwrap().status, UpdateStatus::Failed);

        let store2 = UpdateJobStore::new();
        store2.start("c3", "cmd".to_string()).await.unwrap();
        store2.fail("c3", "exec error".to_string()).await;
        let job = store2.get("c3").await.unwrap();
        assert_eq!(job.status, UpdateStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("exec error"));
    }
}

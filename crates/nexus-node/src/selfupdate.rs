//! Applying a panel update from the panel.
//!
//! Updating means replacing the running binary and restarting the service, so
//! the process that starts the job is not around to finish it. Two things
//! follow, and they shape everything here:
//!
//! * **The work cannot run as a child of the node.** systemd kills a unit's
//!   whole control group on restart, so a spawned child would be killed
//!   half-way through replacing the binary. The updater runs in its own
//!   transient unit (`systemd-run`), outliving the node deliberately.
//! * **Progress cannot live in memory.** The node dies mid-job, so the status
//!   and log are files. The panel reads them back after the node returns,
//!   which is also how it reports an update that failed to come back up.
//!
//! The updater is a shell script rather than Rust for the same reason: by the
//! time it matters, the Rust that would have run it has been replaced on disk.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Subdirectory of `DATA_DIR` holding the updater's state.
const UPDATE_SUBDIR: &str = ".nexus/update";

/// The updater script, written out at update time.
const UPDATER_SCRIPT: &str = include_str!("../scripts/self-update.sh");

/// Transient systemd unit the updater runs as.
const UPDATE_UNIT: &str = "nexus-node-update";

/// How a self-update ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfUpdateStatus {
    /// Running now — or the node died with it running and nothing wrote a
    /// result, which the panel presents the same way until it resolves.
    Running,
    /// The new binary is installed and the service came back up.
    Succeeded,
    /// The update failed before replacing anything; the old binary still runs.
    Failed,
    /// The new binary was installed but would not start, so the previous one
    /// was put back. This is a failure that left a working node behind.
    RolledBack,
}

impl SelfUpdateStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, SelfUpdateStatus::Running)
    }
}

/// A self-update's state, as the updater script writes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfUpdateJob {
    pub status: SelfUpdateStatus,
    /// What was running when the update started.
    pub from_version: String,
    /// Channel the update followed.
    pub channel: String,
    /// Exit code of the underlying installer run, once it has one.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// A human-readable reason, set when something went wrong.
    #[serde(default)]
    pub error: Option<String>,
    pub started_at: u64,
    #[serde(default)]
    pub finished_at: Option<u64>,
    /// Tail of the updater's output. Not persisted — read from the log file.
    #[serde(skip_deserializing, default)]
    pub log: String,
}

fn unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Reads and writes the updater's on-disk state.
#[derive(Clone)]
pub struct SelfUpdater {
    dir: PathBuf,
}

/// Why a self-update cannot be started right now.
#[derive(Debug)]
pub enum StartError {
    /// Another update is already in flight.
    AlreadyRunning,
    /// This node cannot restart itself.
    Unsupported(String),
    /// Something went wrong setting the update up.
    Failed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::AlreadyRunning => write!(f, "an update is already running"),
            StartError::Unsupported(why) => write!(f, "{}", why),
            StartError::Failed(why) => write!(f, "{}", why),
        }
    }
}

impl SelfUpdater {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(UPDATE_SUBDIR),
        }
    }

    fn status_path(&self) -> PathBuf {
        self.dir.join("status.json")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("update.log")
    }

    fn script_path(&self) -> PathBuf {
        self.dir.join("self-update.sh")
    }

    /// The most recent update, with its log.
    pub fn current(&self) -> Option<SelfUpdateJob> {
        let bytes = std::fs::read(self.status_path()).ok()?;
        let mut job: SelfUpdateJob = serde_json::from_slice(&bytes).ok()?;
        job.log = std::fs::read_to_string(self.log_path()).unwrap_or_default();

        // The updater outlives this process, so a job that still reads
        // "running" while nothing is running means the update died with the
        // machine. Report that rather than a spinner that never resolves.
        if job.status == SelfUpdateStatus::Running && !updater_is_running() {
            job.status = SelfUpdateStatus::Failed;
            job.error = Some(
                "the updater stopped without recording a result — check the log below".to_string(),
            );
            job.finished_at = Some(unix_secs());
        }

        Some(job)
    }

    /// Launch an update in a transient unit that outlives this process.
    ///
    /// Returns as soon as the updater is running: the caller cannot wait for
    /// it, because finishing it means killing the caller.
    pub fn start(&self, channel: &str, from_version: &str) -> Result<SelfUpdateJob, StartError> {
        if let Some(existing) = self.current() {
            if existing.status == SelfUpdateStatus::Running {
                return Err(StartError::AlreadyRunning);
            }
        }

        // Without systemd there is no way to restart the service from outside
        // our own (about to be replaced) process.
        if !has_systemd() {
            return Err(StartError::Unsupported(
                "in-panel updates need systemd, which this node does not appear to use — \
                 run install.sh --update on the host instead"
                    .to_string(),
            ));
        }

        std::fs::create_dir_all(&self.dir)
            .map_err(|e| StartError::Failed(format!("could not create {:?}: {}", self.dir, e)))?;

        // Write the updater fresh each time: the copy on disk must match the
        // binary asking for it, not whatever an older version left behind.
        let script = self.script_path();
        std::fs::write(&script, UPDATER_SCRIPT)
            .map_err(|e| StartError::Failed(format!("could not write the updater: {}", e)))?;
        set_executable(&script).map_err(|e| {
            StartError::Failed(format!("could not make the updater runnable: {}", e))
        })?;

        let job = SelfUpdateJob {
            status: SelfUpdateStatus::Running,
            from_version: from_version.to_string(),
            channel: channel.to_string(),
            exit_code: None,
            error: None,
            started_at: unix_secs(),
            finished_at: None,
            log: String::new(),
        };
        self.write_status(&job)?;
        // Start from an empty log so the panel never shows the last run's.
        let _ = std::fs::write(self.log_path(), "");

        // `--collect` reaps the unit when it finishes, so a second update is
        // not blocked by the first unit's leftover record.
        let mut command = std::process::Command::new("systemd-run");
        command
            .arg("--unit")
            .arg(UPDATE_UNIT)
            .arg("--collect")
            .arg("--description=Nexus Panel self-update");

        // A transient unit starts from systemd's environment, not ours, so a
        // node that reaches the internet through a proxy would otherwise fail
        // to download anything the moment it tried to update itself.
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "no_proxy",
        ] {
            if let Ok(value) = std::env::var(key) {
                if !value.trim().is_empty() {
                    command.arg("--setenv").arg(format!("{}={}", key, value));
                }
            }
        }

        let output = command
            .arg(&script)
            .arg(self.status_path())
            .arg(self.log_path())
            .arg(channel)
            .output()
            .map_err(|e| StartError::Failed(format!("could not run systemd-run: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr.trim().lines().last().unwrap_or("no output").to_string();
            let mut failed = job;
            failed.status = SelfUpdateStatus::Failed;
            failed.error = Some(format!("could not start the updater: {}", reason));
            failed.finished_at = Some(unix_secs());
            let _ = self.write_status(&failed);
            return Err(StartError::Failed(format!(
                "could not start the updater: {}",
                reason
            )));
        }

        Ok(self.current().unwrap_or_else(|| SelfUpdateJob {
            status: SelfUpdateStatus::Running,
            from_version: from_version.to_string(),
            channel: channel.to_string(),
            exit_code: None,
            error: None,
            started_at: unix_secs(),
            finished_at: None,
            log: String::new(),
        }))
    }

    fn write_status(&self, job: &SelfUpdateJob) -> Result<(), StartError> {
        let bytes = serde_json::to_vec_pretty(job)
            .map_err(|e| StartError::Failed(format!("could not encode update status: {}", e)))?;
        std::fs::write(self.status_path(), bytes)
            .map_err(|e| StartError::Failed(format!("could not record update status: {}", e)))
    }
}

/// Whether the updater's transient unit is currently active.
fn updater_is_running() -> bool {
    std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", UPDATE_UNIT])
        // A host without systemd prints to stderr on every call; the answer
        // (it is not running) is the same and does not need narrating.
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether this host is running systemd, which the updater needs.
fn has_systemd() -> bool {
    Path::new("/run/systemd/system").is_dir()
}

fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn updater(dir: &Path) -> SelfUpdater {
        SelfUpdater::new(dir)
    }

    #[test]
    fn no_update_has_no_status() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(updater(tmp.path()).current().is_none());
    }

    /// The updater outlives the node, so the node can come back to find a job
    /// still marked running. If nothing is actually running, that is a crashed
    /// update — presenting it as in-progress would spin forever.
    #[test]
    fn a_running_job_with_no_updater_is_reported_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let u = updater(tmp.path());
        std::fs::create_dir_all(&u.dir).unwrap();

        let job = SelfUpdateJob {
            status: SelfUpdateStatus::Running,
            from_version: "0.1.1".to_string(),
            channel: "stable".to_string(),
            exit_code: None,
            error: None,
            started_at: 1,
            finished_at: None,
            log: String::new(),
        };
        std::fs::write(u.status_path(), serde_json::to_vec(&job).unwrap()).unwrap();
        std::fs::write(u.log_path(), "building...\n").unwrap();

        let read = u.current().expect("status should be readable");
        assert_eq!(read.status, SelfUpdateStatus::Failed);
        assert!(read.error.is_some());
        assert!(read.finished_at.is_some());
        // The log survives, because it is the only evidence of what happened.
        assert_eq!(read.log, "building...\n");
    }

    /// A finished job is reported exactly as the updater left it.
    #[test]
    fn a_finished_job_is_read_back_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let u = updater(tmp.path());
        std::fs::create_dir_all(&u.dir).unwrap();

        let job = SelfUpdateJob {
            status: SelfUpdateStatus::RolledBack,
            from_version: "0.1.1".to_string(),
            channel: "main".to_string(),
            exit_code: Some(1),
            error: Some("new binary would not start".to_string()),
            started_at: 1,
            finished_at: Some(2),
            log: String::new(),
        };
        std::fs::write(u.status_path(), serde_json::to_vec(&job).unwrap()).unwrap();

        let read = u.current().unwrap();
        assert_eq!(read.status, SelfUpdateStatus::RolledBack);
        assert_eq!(read.exit_code, Some(1));
        assert_eq!(read.error.as_deref(), Some("new binary would not start"));
        assert!(read.status.is_terminal());
    }

    /// The updater is a shell script and the reader is Rust, so the status
    /// file is a contract between two languages with nothing checking it.
    /// This runs the real script down its failure path and parses what it
    /// wrote with the real type: if the script's JSON ever drifts, the panel
    /// would silently show no result for a real update.
    #[test]
    fn the_script_writes_a_status_this_code_can_read() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/self-update.sh");
        let tmp = tempfile::tempdir().unwrap();
        let u = updater(tmp.path());
        std::fs::create_dir_all(&u.dir).unwrap();

        // Seed the status the node writes before launching the updater.
        let started = SelfUpdateJob {
            status: SelfUpdateStatus::Running,
            from_version: "0.1.1".to_string(),
            channel: "stable".to_string(),
            exit_code: None,
            error: None,
            started_at: 1234,
            finished_at: None,
            log: String::new(),
        };
        std::fs::write(
            u.status_path(),
            serde_json::to_vec_pretty(&started).unwrap(),
        )
        .unwrap();
        std::fs::write(u.log_path(), "").unwrap();

        // Point the installer download at nothing so the script takes its
        // failure path immediately, without building anything.
        let output = std::process::Command::new("bash")
            .arg(script)
            .arg(u.status_path())
            .arg(u.log_path())
            .arg("stable")
            .env("NEXUS_INSTALL_URL", "file:///nexus-does-not-exist")
            .output()
            .expect("the updater script should run");
        assert!(!output.status.success(), "the script should have failed");

        let written = std::fs::read(u.status_path()).expect("status file should exist");
        let job: SelfUpdateJob = serde_json::from_slice(&written).unwrap_or_else(|e| {
            panic!(
                "script wrote unreadable status: {}\n{}",
                e,
                String::from_utf8_lossy(&written)
            )
        });

        assert_eq!(job.status, SelfUpdateStatus::Failed);
        assert_eq!(job.from_version, "0.1.1", "the original version was lost");
        assert_eq!(job.channel, "stable");
        assert_eq!(job.started_at, 1234, "the start time was lost");
        assert!(job.finished_at.is_some());
        let error = job.error.expect("a failure must say why");
        assert!(
            error.contains("installer"),
            "unhelpful failure reason: {}",
            error
        );

        // And the log records what happened, since it is the only evidence
        // once the node has restarted.
        let log = std::fs::read_to_string(u.log_path()).unwrap();
        assert!(
            log.contains("[nexus]"),
            "the script logged nothing: {:?}",
            log
        );
    }

    /// Every status the script can write must round-trip, or the panel shows
    /// nothing at all for an update that did happen.
    #[test]
    fn every_status_round_trips_through_json() {
        for (status, text) in [
            (SelfUpdateStatus::Running, "running"),
            (SelfUpdateStatus::Succeeded, "succeeded"),
            (SelfUpdateStatus::Failed, "failed"),
            (SelfUpdateStatus::RolledBack, "rolled_back"),
        ] {
            let encoded = serde_json::to_string(&status).unwrap();
            assert_eq!(encoded, format!("\"{}\"", text));
            let decoded: SelfUpdateStatus = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, status);
        }
    }
}

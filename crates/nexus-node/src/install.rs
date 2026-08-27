//! Game-file installation.
//!
//! A blueprint's container image supplies the *tooling* a game needs — a JVM,
//! a .NET runtime, the 32-bit libraries SteamCMD links against — but not the
//! game. `ghcr.io/parkervcp/steamcmd:debian` does not even contain SteamCMD.
//! Something has to fetch the game itself into the server's data directory
//! before the server is started for the first time, or the container comes up,
//! fails to exec its startup command, and the file manager shows an empty
//! directory.
//!
//! That is what this module is: deriving *what* to run from the blueprint, and
//! turning it into a shell script that runs once, in its own container, with
//! the server's directory mounted into it.
//!
//! # Where the install comes from
//!
//! In precedence order:
//!
//! 1. an explicit `install` block — the honest declaration, and what an
//!    imported Pterodactyl egg produces;
//! 2. the `startup.lifecycle.pre_start` actions, which is how the shipped
//!    blueprints spell it;
//! 3. the `updates.apply` strategy, since installing and updating game files
//!    are the same operation on an empty directory.
//!
//! A blueprint that declares none of these needs no install: its image is
//! self-contained, and the server is ready to start as soon as it is created.

use crate::update::{build_update_command, DEFAULT_INSTALL_DIR};
use nexus_config::{GameConfig, LifecycleAction};
use std::collections::HashMap;
use std::time::Duration;

/// How long an install may run when the blueprint does not say.
///
/// Game downloads are measured in gigabytes over whatever link the node has;
/// this is a backstop against a wedged install, not a normal bound.
pub const DEFAULT_INSTALL_TIMEOUT: Duration = Duration::from_secs(3600);

/// Interpreter used when a blueprint does not name one.
const DEFAULT_ENTRYPOINT: &str = "/bin/sh";

/// Where SteamCMD is fetched from. Valve's own installer tarball, the same one
/// every Pterodactyl SteamCMD egg uses.
const STEAMCMD_URL: &str = "https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz";

/// Where DepotDownloader is fetched from, when a blueprint asks for it.
const DEPOTDOWNLOADER_URL: &str =
    "https://github.com/SteamRE/DepotDownloader/releases/latest/download/DepotDownloader-linux-x64.zip";

/// A resolved, runnable installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// Image to run the install in.
    pub image: String,
    /// Interpreter the script is fed to.
    pub entrypoint: String,
    /// The script itself, with blueprint variables already substituted.
    pub script: String,
    /// Where the server's data directory is mounted inside the container, and
    /// the script's working directory.
    pub server_dir: String,
    /// Environment the script runs with (blueprint environment + variables).
    pub env: HashMap<String, String>,
    /// How long the install may run.
    pub timeout: Duration,
}

impl InstallPlan {
    /// The argv this plan runs as.
    pub fn argv(&self) -> Vec<String> {
        vec![
            self.entrypoint.clone(),
            "-c".to_string(),
            self.script.clone(),
        ]
    }
}

/// Substitute `{{VARIABLE}}` placeholders using `vars`.
///
/// Blueprints template their commands, URLs and arguments this way. An unknown
/// placeholder is left as-is rather than blanked: a literal `{{MC_VERSION}}` in
/// a failing download URL says what went wrong, where an empty string produces
/// a mystery 404.
pub fn substitute(input: &str, vars: &HashMap<String, String>) -> String {
    // Cheap bail-out for the overwhelmingly common case.
    if !input.contains("{{") {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];

        let Some(end) = after.find("}}") else {
            // Unterminated placeholder: nothing more to substitute.
            out.push_str(&rest[start..]);
            return out;
        };

        let name = after[..end].trim();
        match vars.get(name) {
            Some(value) => out.push_str(value),
            None => {
                out.push_str("{{");
                out.push_str(&after[..end]);
                out.push_str("}}");
            }
        }
        rest = &after[end + 2..];
    }

    out.push_str(rest);
    out
}

/// The variables a blueprint's own scripts are rendered with: its container
/// environment, overlaid with the declared variables' defaults.
pub fn blueprint_vars(config: &GameConfig) -> HashMap<String, String> {
    let mut vars = config.container.environment.clone();
    for var in &config.variables {
        vars.insert(var.name.clone(), var.default.clone());
    }
    vars
}

/// The directory a blueprint's game files live in inside the container.
pub fn working_dir(config: &GameConfig) -> String {
    let dir = config.startup.working_dir.trim();
    if dir.is_empty() {
        DEFAULT_INSTALL_DIR.to_string()
    } else {
        dir.to_string()
    }
}

/// Work out how to install this blueprint's game files.
///
/// Returns `None` when the blueprint declares no install of any kind, which
/// means its image is self-contained and the server can be started as soon as
/// it is created.
pub fn plan(config: &GameConfig) -> Option<InstallPlan> {
    let vars = blueprint_vars(config);
    let server_dir = working_dir(config);

    let (image, entrypoint, mut script, dir, timeout) = match &config.install {
        // 1. An explicit install block.
        Some(install) if !install.script.trim().is_empty() => (
            install.image.clone().unwrap_or_else(|| config.container.image.clone()),
            install.entrypoint.clone().unwrap_or_else(|| DEFAULT_ENTRYPOINT.to_string()),
            install.script.clone(),
            install.server_dir.clone().unwrap_or_else(|| server_dir.clone()),
            install
                .timeout
                .as_deref()
                .and_then(nexus_config::parse_duration)
                .unwrap_or(DEFAULT_INSTALL_TIMEOUT),
        ),
        _ => {
            // 2. The pre-start lifecycle actions.
            let from_lifecycle = script_from_lifecycle(config);
            let script = match from_lifecycle {
                Some(script) => script,
                // 3. The update strategy, applied to an empty directory.
                None => script_from_updates(config, &server_dir)?,
            };
            (
                config.container.image.clone(),
                DEFAULT_ENTRYPOINT.to_string(),
                script,
                server_dir.clone(),
                lifecycle_timeout(config).unwrap_or(DEFAULT_INSTALL_TIMEOUT),
            )
        }
    };

    script = substitute(&script, &vars);
    let script = wrap_script(&script, &dir);

    Some(InstallPlan {
        image,
        entrypoint,
        script,
        server_dir: dir,
        env: vars,
        timeout,
    })
}

/// Build an install script out of a blueprint's `pre_start` actions.
///
/// Only the actions that *do* something to the server's files are install
/// steps; a health check or a console command is part of running the server,
/// not of installing it.
///
/// Every qualifying action runs, whatever its `condition`. Installing is the
/// first start, so `first_start` holds by definition, and the richer
/// expressions the blueprints use (`!file_exists('paper.jar') || …`) are
/// likewise true on an empty directory. Evaluating them properly is a job for
/// whatever runs `pre_start` on *every* start, which is a separate thing.
fn script_from_lifecycle(config: &GameConfig) -> Option<String> {
    let lifecycle = config.startup.lifecycle.as_ref()?;

    let steps: Vec<String> = lifecycle
        .pre_start
        .iter()
        .filter_map(|action| match action {
            LifecycleAction::Execute { command, .. } if !command.trim().is_empty() => {
                Some(command.trim().to_string())
            }
            LifecycleAction::Download {
                url, destination, ..
            } if !url.trim().is_empty() => Some(download_step(url.trim(), destination.as_deref())),
            _ => None,
        })
        .collect();

    if steps.is_empty() {
        None
    } else {
        Some(steps.join("\n\n"))
    }
}

/// The longest timeout any `pre_start` action asks for.
fn lifecycle_timeout(config: &GameConfig) -> Option<Duration> {
    let lifecycle = config.startup.lifecycle.as_ref()?;
    lifecycle
        .pre_start
        .iter()
        .filter_map(|action| match action {
            LifecycleAction::Execute { timeout, .. } => {
                timeout.as_deref().and_then(nexus_config::parse_duration)
            }
            _ => None,
        })
        .max()
}

/// Fall back to the blueprint's update strategy.
///
/// Installing game files into an empty directory and updating them in place
/// are the same SteamCMD (or DepotDownloader, or download) invocation, so a
/// blueprint that declares how to update already says how to install.
fn script_from_updates(config: &GameConfig, install_dir: &str) -> Option<String> {
    let updates = config.updates.as_ref()?;
    build_update_command(&updates.apply, install_dir).ok()
}

/// A `download` lifecycle action as a shell step.
fn download_step(url: &str, destination: Option<&str>) -> String {
    match destination.map(str::trim).filter(|d| !d.is_empty()) {
        Some(dest) => format!(
            "mkdir -p \"$(dirname {dest})\"\ncurl -fsSL --retry 3 -o {dest} {url}",
            dest = shell_quote(dest),
            url = shell_quote(url)
        ),
        // No destination: keep the remote file name, as `curl -O` does.
        None => format!("curl -fsSL --retry 3 -O {}", shell_quote(url)),
    }
}

/// Quote a string for safe inclusion in a `/bin/sh` command line.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Whether a script leans on SteamCMD.
fn needs_steamcmd(script: &str) -> bool {
    script.contains("steamcmd")
}

/// Whether a script leans on DepotDownloader.
fn needs_depot_downloader(script: &str) -> bool {
    script.contains("DepotDownloader")
}

/// Wrap an install script with the preamble that makes it runnable.
///
/// The preamble does three things:
///
/// * fails the whole install on the first failing step, so a server is never
///   left half-installed and reported as ready;
/// * enters the server directory, which is what every blueprint's script
///   assumes it is in;
/// * installs the download tools the script needs. This is the part without
///   which the shipped blueprints cannot work: they all call
///   `./steamcmd/steamcmd.sh`, and no game image ships SteamCMD — Pterodactyl
///   downloads it in its own install container, which is exactly what this is.
fn wrap_script(script: &str, server_dir: &str) -> String {
    let mut out = String::new();

    out.push_str("set -e\n");
    out.push_str(&format!(
        "mkdir -p {dir}\ncd {dir}\n",
        dir = shell_quote(server_dir)
    ));

    // A writable place for shims, so a script can call `steamcmd` by name
    // whether or not the image has one on PATH.
    out.push_str("NEXUS_BIN=\"${TMPDIR:-/tmp}/nexus-install-bin\"\n");
    out.push_str("mkdir -p \"$NEXUS_BIN\"\nPATH=\"$NEXUS_BIN:$PATH\"\nexport PATH\n");

    if needs_steamcmd(script) {
        out.push_str(&steamcmd_preamble(server_dir));
    }
    if needs_depot_downloader(script) {
        out.push_str(&depot_downloader_preamble(server_dir));
    }

    out.push_str("\necho '[nexus] running install script'\n");
    out.push_str(script);
    out.push('\n');
    out.push_str("\necho '[nexus] install script finished'\n");
    out
}

/// Install SteamCMD into the server directory if it is not already usable.
///
/// Both spellings the blueprints use are made to work: the literal
/// `./steamcmd/steamcmd.sh` path, and a bare `steamcmd` on `PATH`.
fn steamcmd_preamble(server_dir: &str) -> String {
    format!(
        r#"
if [ ! -x ./steamcmd/steamcmd.sh ]; then
  echo '[nexus] installing SteamCMD'
  mkdir -p ./steamcmd
  curl -fsSL --retry 3 {url} | tar -xz -C ./steamcmd
fi
# Let a script call `steamcmd` by name as well as by path.
if ! command -v steamcmd >/dev/null 2>&1; then
  printf '#!/bin/sh\nexec %s/steamcmd/steamcmd.sh "$@"\n' {dir} > "$NEXUS_BIN/steamcmd"
  chmod +x "$NEXUS_BIN/steamcmd"
fi
"#,
        url = shell_quote(STEAMCMD_URL),
        dir = shell_quote(server_dir),
    )
}

/// Install DepotDownloader into the server directory if it is not present.
fn depot_downloader_preamble(server_dir: &str) -> String {
    format!(
        r#"
if ! command -v DepotDownloader >/dev/null 2>&1 && [ ! -x ./DepotDownloader/DepotDownloader ]; then
  echo '[nexus] installing DepotDownloader'
  mkdir -p ./DepotDownloader
  curl -fsSL --retry 3 -o ./DepotDownloader/dd.zip {url}
  # Slim images often lack unzip; Python's zipfile is the usual fallback, and
  # DepotDownloader's own image has a .NET runtime that implies neither.
  if command -v unzip >/dev/null 2>&1; then
    unzip -oq ./DepotDownloader/dd.zip -d ./DepotDownloader
  elif command -v python3 >/dev/null 2>&1; then
    python3 -m zipfile -e ./DepotDownloader/dd.zip ./DepotDownloader
  else
    echo '[nexus] cannot unpack DepotDownloader: install unzip in this image' >&2
    exit 1
  fi
  rm -f ./DepotDownloader/dd.zip
  chmod +x ./DepotDownloader/DepotDownloader
fi
if ! command -v DepotDownloader >/dev/null 2>&1; then
  printf '#!/bin/sh\nexec %s/DepotDownloader/DepotDownloader "$@"\n' {dir} > "$NEXUS_BIN/DepotDownloader"
  chmod +x "$NEXUS_BIN/DepotDownloader"
fi
"#,
        url = shell_quote(DEPOTDOWNLOADER_URL),
        dir = shell_quote(server_dir),
    )
}

// ---------------------------------------------------------------------------
// Install jobs
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;

/// Where a server stands with respect to its game files.
///
/// This is persisted with the server's state, so it survives a node restart —
/// otherwise a reboot mid-install would leave a half-populated directory that
/// the panel happily reports as ready to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    /// Not recorded — a server created before installs were tracked.
    ///
    /// Such a server was startable, and must stay startable across the
    /// upgrade; the node works out what it really is when it next restores
    /// state, from whether the server has any files.
    #[default]
    Unknown,
    /// The blueprint declares an install that has not run yet.
    Pending,
    /// An install is running now.
    Running,
    /// Game files are in place; the server can start.
    Installed,
    /// The last install failed. The server will not start until one succeeds.
    Failed,
    /// This blueprint needs no install — its image is self-contained.
    NotRequired,
}

impl InstallState {
    /// Whether a server in this state may be started.
    pub fn can_start(self) -> bool {
        matches!(
            self,
            InstallState::Installed | InstallState::NotRequired | InstallState::Unknown
        )
    }

    /// Why a server in this state cannot be started, for the operator.
    pub fn blocked_reason(self) -> Option<&'static str> {
        match self {
            InstallState::Pending => {
                Some("this server's game files have not been installed yet — run the install first")
            }
            InstallState::Running => Some("this server's game files are still being installed"),
            InstallState::Failed => {
                Some("this server's last install failed — check the install log and run it again")
            }
            // A server from before install tracking is left alone: it was
            // startable yesterday and blocking it today would be a regression.
            InstallState::Installed | InstallState::NotRequired | InstallState::Unknown => None,
        }
    }
}

/// Lifecycle status of a background install job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStatus {
    Running,
    Succeeded,
    Failed,
}

/// A snapshot of an install job, safe to serialize to the API.
#[derive(Debug, Clone, Serialize)]
pub struct InstallJob {
    /// Unique job id.
    pub id: String,
    /// Server this install belongs to.
    pub container_id: String,
    pub status: InstallStatus,
    /// Image the install ran in.
    pub image: String,
    /// Output captured from the install container, newest last.
    pub log: String,
    /// Process exit code, once finished.
    pub exit_code: Option<i32>,
    /// Why the job could not run at all, as distinct from a non-zero exit.
    pub error: Option<String>,
    /// Unix seconds when the install started.
    pub started_at: u64,
    /// Unix seconds when it finished.
    pub finished_at: Option<u64>,
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl InstallJob {
    fn new(id: String, container_id: String, image: String) -> Self {
        Self {
            id,
            container_id,
            status: InstallStatus::Running,
            image,
            log: String::new(),
            exit_code: None,
            error: None,
            started_at: unix_secs(SystemTime::now()),
            finished_at: None,
        }
    }
}

/// In-memory registry of install jobs, one per server.
#[derive(Default)]
pub struct InstallJobStore {
    jobs: RwLock<HashMap<String, InstallJob>>,
}

impl InstallJobStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a running install for `container_id`.
    ///
    /// Fails if one is already running: two SteamCMD runs writing the same
    /// directory corrupt each other's downloads.
    pub async fn start(&self, container_id: &str, image: &str) -> Result<InstallJob, String> {
        let mut jobs = self.jobs.write().await;
        if let Some(existing) = jobs.get(container_id) {
            if existing.status == InstallStatus::Running {
                return Err("an install is already running for this server".to_string());
            }
        }
        let id = format!("ins-{}", uuid::Uuid::new_v4());
        let job = InstallJob::new(id, container_id.to_string(), image.to_string());
        jobs.insert(container_id.to_string(), job.clone());
        Ok(job)
    }

    /// Get the current job snapshot for a server, if any.
    pub async fn get(&self, container_id: &str) -> Option<InstallJob> {
        self.jobs.read().await.get(container_id).cloned()
    }

    /// Append output to a running job, so the panel can follow a long install
    /// rather than waiting for a final blob.
    pub async fn append_log(&self, container_id: &str, chunk: &str) {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(container_id) {
            job.log.push_str(chunk);
        }
    }

    /// Mark a job finished with its exit code.
    pub async fn finish(&self, container_id: &str, exit_code: Option<i32>) {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(container_id) {
            job.exit_code = exit_code;
            job.status = if exit_code == Some(0) {
                InstallStatus::Succeeded
            } else {
                InstallStatus::Failed
            };
            job.finished_at = Some(unix_secs(SystemTime::now()));
        }
    }

    /// Mark a job that never got as far as running.
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
pub type SharedInstallJobStore = Arc<InstallJobStore>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Load a blueprint that ships with the panel.
    fn blueprint(name: &str) -> GameConfig {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../blueprints/");
        let yaml = std::fs::read_to_string(format!("{}{}.yaml", path, name))
            .unwrap_or_else(|e| panic!("reading blueprint {}: {}", name, e));
        GameConfig::from_yaml(&yaml).unwrap_or_else(|e| panic!("parsing {}: {}", name, e))
    }

    #[test]
    fn substitutes_declared_variables() {
        let mut vars = HashMap::new();
        vars.insert("SERVER_PORT".to_string(), "2302".to_string());
        vars.insert("MODS".to_string(), "@CF".to_string());

        assert_eq!(
            substitute("-port={{SERVER_PORT}} -mod={{MODS}}", &vars),
            "-port=2302 -mod=@CF"
        );
        // Whitespace inside the braces is tolerated.
        assert_eq!(substitute("{{ SERVER_PORT }}", &vars), "2302");
        // Text with no placeholders is returned unchanged.
        assert_eq!(substitute("plain", &vars), "plain");
    }

    /// An unknown placeholder is left visible. Blanking it would turn a
    /// missing variable into a silently malformed URL or argument.
    #[test]
    fn leaves_unknown_placeholders_alone() {
        let vars = HashMap::new();
        assert_eq!(substitute("v{{NOPE}}!", &vars), "v{{NOPE}}!");
        assert_eq!(substitute("{{unterminated", &vars), "{{unterminated");
    }

    /// Every shipped blueprint must produce a runnable install — that is what
    /// "any game installs" means, and a blueprint that yields none would give
    /// an empty server directory and a server that cannot start.
    #[test]
    fn every_shipped_blueprint_has_an_install() {
        for name in [
            "cs2",
            "dayz",
            "minecraft-paper",
            "palworld",
            "rust",
            "rust-carbon",
            "valheim",
        ] {
            let plan =
                plan(&blueprint(name)).unwrap_or_else(|| panic!("{} yields no install plan", name));

            assert!(!plan.script.trim().is_empty(), "{}: empty script", name);
            assert!(plan.server_dir.starts_with('/'), "{}: relative dir", name);
            assert!(!plan.image.is_empty(), "{}: no image", name);
            assert!(
                plan.script.contains("set -e"),
                "{}: a failing step must abort the install",
                name
            );
            assert!(
                plan.script.contains(&format!("cd '{}'", plan.server_dir)),
                "{}: install must run in the server directory",
                name
            );
        }
    }

    /// The blueprints call `./steamcmd/steamcmd.sh`, and no game image ships
    /// SteamCMD — `ghcr.io/parkervcp/steamcmd:debian` does not contain it.
    /// Without the bootstrap every SteamCMD install fails on "not found".
    #[test]
    fn steamcmd_installs_bootstrap_the_tool() {
        for name in ["cs2", "dayz", "palworld", "rust", "valheim"] {
            let plan = plan(&blueprint(name)).unwrap();
            assert!(
                plan.script.contains("installing SteamCMD"),
                "{}: install does not bootstrap SteamCMD",
                name
            );
            assert!(
                plan.script.contains("steamcmd_linux.tar.gz"),
                "{}: no SteamCMD download",
                name
            );
        }
    }

    /// Likewise DepotDownloader, which the Carbon blueprint uses.
    #[test]
    fn depot_downloader_installs_bootstrap_the_tool() {
        let plan = plan(&blueprint("rust-carbon")).unwrap();
        assert!(plan.script.contains("installing DepotDownloader"));
        assert!(plan.script.contains("DepotDownloader-linux-x64.zip"));
        // Carbon's second pre-start step must survive too.
        assert!(
            plan.script.contains("Carbon"),
            "later install steps were dropped"
        );
    }

    /// A blueprint that installs by downloading a file gets its templated URL
    /// resolved — a literal `{{MC_VERSION}}` in the URL would 404.
    #[test]
    fn download_installs_resolve_their_url() {
        let config = blueprint("minecraft-paper");
        let plan = plan(&config).unwrap();
        assert!(plan.script.contains("curl"), "download step missing");
        assert!(
            !plan.script.contains("{{MC_VERSION}}"),
            "template variable left unresolved in: {}",
            plan.script
        );
        assert!(
            plan.script.contains("paper.jar"),
            "destination not honoured"
        );
    }

    /// An explicit `install` block wins over everything else, and carries its
    /// own image, interpreter, directory and timeout.
    #[test]
    fn an_explicit_install_block_takes_precedence() {
        let mut config = blueprint("dayz");
        config.install = Some(nexus_config::Install {
            image: Some("docker.io/library/debian:bookworm-slim".to_string()),
            entrypoint: Some("bash".to_string()),
            script: "echo installing {{SERVER_PORT}}".to_string(),
            server_dir: Some("/mnt/server".to_string()),
            timeout: Some("120s".to_string()),
        });

        let plan = plan(&config).unwrap();
        assert_eq!(plan.image, "docker.io/library/debian:bookworm-slim");
        assert_eq!(plan.entrypoint, "bash");
        assert_eq!(plan.server_dir, "/mnt/server");
        assert_eq!(plan.timeout, Duration::from_secs(120));
        assert!(plan.script.contains("echo installing 2302"));
        // The lifecycle's SteamCMD step is not mixed in.
        assert!(!plan.script.contains("223350"));
    }

    /// With no install block and no lifecycle hooks, the update strategy is
    /// the install: on an empty directory they are the same operation.
    #[test]
    fn falls_back_to_the_update_strategy() {
        let mut config = blueprint("valheim");
        config.startup.lifecycle = None;

        let plan = plan(&config).unwrap();
        assert!(
            plan.script.contains("896660"),
            "app id missing: {}",
            plan.script
        );
        assert!(plan.script.contains("installing SteamCMD"));
    }

    /// A self-contained image needs no install, and such a server must be
    /// startable immediately rather than waiting for a step that never runs.
    #[test]
    fn a_blueprint_with_nothing_to_install_has_no_plan() {
        let mut config = blueprint("valheim");
        config.install = None;
        config.startup.lifecycle = None;
        config.updates = None;

        assert!(plan(&config).is_none());
    }

    /// Only file-touching actions are install steps. A health check or a
    /// console command belongs to running the server, and running one at
    /// install time would either hang or fail.
    #[test]
    fn non_install_lifecycle_actions_are_ignored() {
        let mut config = blueprint("valheim");
        config.updates = None;
        config.startup.lifecycle = Some(nexus_config::Lifecycle {
            pre_start: vec![
                nexus_config::LifecycleAction::HealthCheck {
                    endpoint: "http://localhost/health".to_string(),
                    timeout: "5s".to_string(),
                },
                nexus_config::LifecycleAction::SendCommand {
                    command: "save".to_string(),
                    delay: None,
                },
            ],
            post_start: vec![],
            pre_stop: vec![],
        });

        assert!(
            plan(&config).is_none(),
            "non-install actions produced an install"
        );
    }

    /// Blueprint variables reach the install script's environment, which is
    /// how an egg-style script reads its own configuration.
    #[test]
    fn install_env_carries_blueprint_variables() {
        let plan = plan(&blueprint("dayz")).unwrap();
        assert_eq!(
            plan.env.get("SERVER_PORT").map(String::as_str),
            Some("2302")
        );
        // container.environment is included too.
        assert_eq!(
            plan.env.get("SRCDS_APPID").map(String::as_str),
            Some("223350")
        );
    }

    /// Shell metacharacters in a URL must not escape into the script.
    #[test]
    fn download_urls_are_quoted() {
        let step = download_step("https://example.invalid/a;rm -rf /", Some("out.jar"));
        assert!(step.contains("'https://example.invalid/a;rm -rf /'"));
    }

    #[test]
    fn a_plan_runs_as_an_interpreter_invocation() {
        let plan = plan(&blueprint("dayz")).unwrap();
        let argv = plan.argv();
        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        assert_eq!(argv[2], plan.script);
    }

    #[test]
    fn install_state_gates_starting() {
        assert!(InstallState::Installed.can_start());
        assert!(InstallState::NotRequired.can_start());
        // A server predating install tracking stays startable.
        assert!(InstallState::Unknown.can_start());
        assert!(!InstallState::Pending.can_start());
        assert!(!InstallState::Running.can_start());
        assert!(!InstallState::Failed.can_start());
        assert!(InstallState::Failed.blocked_reason().is_some());
        assert!(InstallState::Installed.blocked_reason().is_none());
    }

    #[tokio::test]
    async fn a_second_install_is_refused_while_one_runs() {
        let store = InstallJobStore::new();
        store.start("srv", "img").await.expect("first install starts");
        assert!(
            store.start("srv", "img").await.is_err(),
            "two installs writing the same directory would corrupt each other"
        );

        store.append_log("srv", "downloading\n").await;
        store.finish("srv", Some(0)).await;

        let job = store.get("srv").await.unwrap();
        assert_eq!(job.status, InstallStatus::Succeeded);
        assert_eq!(job.log, "downloading\n");

        // Once finished, the server can be reinstalled.
        assert!(store.start("srv", "img").await.is_ok());
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_a_failed_install() {
        let store = InstallJobStore::new();
        store.start("srv", "img").await.unwrap();
        store.finish("srv", Some(1)).await;
        assert_eq!(
            store.get("srv").await.unwrap().status,
            InstallStatus::Failed
        );
    }
}

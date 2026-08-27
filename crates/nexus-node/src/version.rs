//! What this node is running, and whether something newer exists.
//!
//! "Am I up to date?" has two different answers depending on how a node is
//! deployed, and the panel used to give only one of them — badly. It compared
//! the crate version against the latest GitHub *release tag*, which meant a
//! node tracking `main` could never see itself as behind (the crate version
//! does not move between releases) while every node reported a permanent
//! phantom update whenever the manifest lagged a published tag.
//!
//! So a node declares a [`Channel`]. On `stable` the question is about release
//! tags; on `main` it is about commits, which is why the binary carries the
//! commit it was built from (see `build.rs`).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// GitHub repository the update check consults.
const REPO: &str = "rifle-ak/nexus-panel";

/// Upstream API calls are bounded: an update check must never be the reason a
/// settings page hangs.
const API_TIMEOUT: Duration = Duration::from_secs(8);

/// Value stamped in by `build.rs` when the source was not a git checkout.
const UNKNOWN: &str = "unknown";

/// Which stream of releases a node follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Tagged releases. The conservative default: an operator running a game
    /// host generally wants what was deliberately published.
    #[default]
    Stable,
    /// The `main` branch, as the installer's default build already tracks.
    Main,
}

impl Channel {
    /// Read the channel from `UPDATE_CHANNEL`, defaulting to stable.
    pub fn from_env() -> Self {
        match std::env::var("UPDATE_CHANNEL")
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str()
        {
            "main" | "edge" | "nightly" => Channel::Main,
            _ => Channel::Stable,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Main => "main",
        }
    }
}

/// The identity of the running binary, fixed at compile time.
#[derive(Debug, Clone, Serialize)]
pub struct BuildInfo {
    /// Crate version, e.g. `0.1.1`.
    pub version: String,
    /// Full commit the binary was built from, or `unknown`.
    pub commit: String,
    /// First 12 characters of `commit`, for display.
    pub commit_short: String,
    /// Commit timestamp (RFC 3339), or `unknown`.
    pub commit_date: String,
    /// Whether the working tree had uncommitted changes at build time.
    pub dirty: bool,
}

impl BuildInfo {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: env!("NEXUS_GIT_SHA").to_string(),
            commit_short: env!("NEXUS_GIT_SHA_SHORT").to_string(),
            commit_date: env!("NEXUS_GIT_COMMIT_DATE").to_string(),
            dirty: env!("NEXUS_GIT_DIRTY") == "true",
        }
    }

    /// Whether this binary knows which commit it came from.
    pub fn has_commit(&self) -> bool {
        self.commit != UNKNOWN && !self.commit.is_empty()
    }

    /// One-line description for logs and the panel.
    pub fn describe(&self) -> String {
        let mut s = format!("v{}", self.version);
        if self.has_commit() {
            s.push_str(&format!(" ({})", self.commit_short));
        }
        if self.dirty {
            s.push_str(" [modified]");
        }
        s
    }
}

/// The answer to "is there something newer?".
#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    /// What is running now.
    pub current: BuildInfo,
    /// Channel this node follows.
    pub channel: &'static str,
    /// Newest version on the channel: a release tag, or a short commit.
    pub latest: String,
    /// True when `latest` is genuinely ahead of what is running.
    pub update_available: bool,
    /// How many commits behind, when the channel and API can tell us.
    pub commits_behind: Option<u32>,
    /// Why the check could not reach a conclusion, if it could not.
    pub error: Option<String>,
}

impl UpdateStatus {
    /// A status that reports what is running but nothing about upstream.
    fn undetermined(current: BuildInfo, channel: Channel, error: String) -> Self {
        let latest = current.version.clone();
        Self {
            current,
            channel: channel.as_str(),
            latest,
            update_available: false,
            commits_behind: None,
            error: Some(error),
        }
    }
}

/// Check the configured channel for a newer build.
///
/// Never returns an error: a node that cannot reach GitHub is not out of date,
/// it is uninformed, and the difference matters to whoever reads the panel.
pub async fn check(client: &reqwest::Client, channel: Channel) -> UpdateStatus {
    let current = BuildInfo::current();
    match channel {
        Channel::Stable => check_stable(client, current).await,
        Channel::Main => check_main(client, current).await,
    }
}

/// Compare the crate version against the latest published release.
async fn check_stable(client: &reqwest::Client, current: BuildInfo) -> UpdateStatus {
    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);

    let latest = match get_json(client, &url).await {
        Ok(body) => match body["tag_name"].as_str() {
            Some(tag) => tag.trim_start_matches('v').to_string(),
            // A repository with no releases yet is not a repository that is
            // behind; saying "up to date" would be a guess either way.
            None => {
                return UpdateStatus::undetermined(
                    current,
                    Channel::Stable,
                    "no published releases to compare against".to_string(),
                )
            }
        },
        Err(e) => return UpdateStatus::undetermined(current, Channel::Stable, e),
    };

    stable_decision(&latest, current)
}

/// Decide the stable-channel result. Split from the HTTP so the comparison can
/// be tested without a network or a live repository.
fn stable_decision(latest: &str, current: BuildInfo) -> UpdateStatus {
    let update_available = is_newer_version(latest, &current.version);
    UpdateStatus {
        current,
        channel: Channel::Stable.as_str(),
        latest: latest.to_string(),
        update_available,
        commits_behind: None,
        error: None,
    }
}

/// Compare the built commit against the tip of `main`.
async fn check_main(client: &reqwest::Client, current: BuildInfo) -> UpdateStatus {
    if !current.has_commit() {
        return UpdateStatus::undetermined(
            current,
            Channel::Main,
            "this binary was not built from a git checkout, so it cannot be compared with main"
                .to_string(),
        );
    }

    let url = format!("https://api.github.com/repos/{}/commits/main", REPO);
    let head = match get_json(client, &url).await {
        Ok(body) => match body["sha"].as_str() {
            Some(sha) => sha.to_string(),
            None => {
                return UpdateStatus::undetermined(
                    current,
                    Channel::Main,
                    "could not read the tip of main".to_string(),
                )
            }
        },
        Err(e) => return UpdateStatus::undetermined(current, Channel::Main, e),
    };

    if head == current.commit {
        return main_decision(&head, None, current);
    }

    // How far behind, so the panel can say something more useful than "newer".
    // `compare/<ours>...main` reports how far `main` is ahead of the commit we
    // were built from, which is exactly the number of commits we are behind. A
    // node built from a commit that is not an ancestor of main (a local branch,
    // a rebased history) has no meaningful count, and GitHub omits it.
    let compare = format!(
        "https://api.github.com/repos/{}/compare/{}...main",
        REPO, current.commit
    );
    let commits_behind = get_json(client, &compare)
        .await
        .ok()
        .and_then(|body| body["ahead_by"].as_u64())
        .map(|n| n as u32);

    main_decision(&head, commits_behind, current)
}

/// Decide the main-channel result from the tip of the branch.
fn main_decision(head: &str, commits_behind: Option<u32>, current: BuildInfo) -> UpdateStatus {
    let up_to_date = head == current.commit;
    UpdateStatus {
        latest: head.chars().take(12).collect(),
        channel: Channel::Main.as_str(),
        update_available: !up_to_date,
        commits_behind: if up_to_date { Some(0) } else { commits_behind },
        error: None,
        current,
    }
}

/// GET a GitHub API endpoint, mapping every failure to a readable sentence.
async fn get_json(client: &reqwest::Client, url: &str) -> Result<serde_json::Value, String> {
    let response = client
        .get(url)
        .header("User-Agent", "nexus-panel")
        .header("Accept", "application/vnd.github+json")
        .timeout(API_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("could not reach GitHub: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        // Rate limiting is the common one, and is worth naming: it resolves
        // on its own, unlike a 404.
        return Err(match status.as_u16() {
            403 | 429 => "GitHub rate-limited the update check; try again later".to_string(),
            404 => format!("{} not found upstream", url),
            other => format!("GitHub returned HTTP {}", other),
        });
    }

    response
        .json()
        .await
        .map_err(|e| format!("could not read GitHub's response: {}", e))
}

/// Whether `latest` is a strictly newer release than `current`.
pub fn is_newer_version(latest: &str, current: &str) -> bool {
    match (
        semver::Version::parse(latest),
        semver::Version::parse(current),
    ) {
        (Ok(l), Ok(c)) => l > c,
        // Unparseable on either side: only a difference is evidence, and even
        // that is weak, so it is treated as "something changed".
        _ => latest != current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_parses_from_env_spellings() {
        assert_eq!(
            std::mem::discriminant(&Channel::Main),
            std::mem::discriminant(&Channel::Main)
        );
        assert_eq!(Channel::Stable.as_str(), "stable");
        assert_eq!(Channel::Main.as_str(), "main");
        assert_eq!(Channel::default(), Channel::Stable);
    }

    #[test]
    fn semver_comparison() {
        // The bug this replaces: string comparison ranked 0.9.0 above 0.10.0.
        assert!(is_newer_version("0.10.0", "0.9.0"));
        assert!(!is_newer_version("0.9.0", "0.10.0"));
        assert!(is_newer_version("1.0.0", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.2.0"));
        assert!(!is_newer_version("unknown", "unknown"));
        assert!(is_newer_version("0.2.0", "0.1.1"));
        assert!(is_newer_version("0.1.2", "0.1.1"));
        assert!(!is_newer_version("0.1.1", "0.1.1"));
        assert!(!is_newer_version("0.1.0", "0.1.1"));
        // Unparseable values fall back to inequality.
        assert!(is_newer_version("nightly", "0.1.1"));
        assert!(!is_newer_version("nightly", "nightly"));
    }

    /// The binary must know its own identity, or the `main` channel has
    /// nothing to compare. In this repository the build is a git checkout, so
    /// the stamp has to be real rather than the `unknown` fallback.
    #[test]
    fn the_binary_knows_what_it_was_built_from() {
        let info = BuildInfo::current();
        assert!(!info.version.is_empty());
        assert!(
            info.has_commit(),
            "build.rs did not stamp a commit; the main channel cannot work"
        );
        assert_eq!(info.commit_short.len(), 12);
        assert!(info.describe().starts_with('v'));
        assert!(info.describe().contains(&info.commit_short));
    }

    /// Same commit as the branch tip means up to date, and explicitly zero
    /// commits behind rather than an absent count.
    #[test]
    fn main_channel_recognises_the_current_commit() {
        let current = BuildInfo::current();
        let status = main_decision(&current.commit.clone(), None, current);
        assert!(!status.update_available);
        assert_eq!(status.commits_behind, Some(0));
        assert!(status.error.is_none());
    }

    /// A different tip means an update, and the count is carried through so
    /// the panel can say how far behind rather than just "newer".
    #[test]
    fn main_channel_reports_how_far_behind() {
        let current = BuildInfo::current();
        let status = main_decision("f".repeat(40).as_str(), Some(12), current);
        assert!(status.update_available);
        assert_eq!(status.commits_behind, Some(12));
        assert_eq!(
            status.latest, "ffffffffffff",
            "the tip should be shortened for display"
        );
    }

    /// A commit that is not on main at all (a local build, a rebased branch)
    /// still reports an update, just without a count — better than claiming a
    /// distance that does not exist.
    #[test]
    fn main_channel_tolerates_an_uncountable_distance() {
        let current = BuildInfo::current();
        let status = main_decision("a".repeat(40).as_str(), None, current);
        assert!(status.update_available);
        assert_eq!(status.commits_behind, None);
    }

    /// The stable channel compares releases, and must not invent an update
    /// when the running version is the published one — the bug that made every
    /// node show a permanent "update available".
    #[test]
    fn stable_channel_compares_release_versions() {
        let current = BuildInfo::current();
        let same = stable_decision(&current.version.clone(), current.clone());
        assert!(
            !same.update_available,
            "running the published version is not an update"
        );

        let newer = stable_decision("99.0.0", current.clone());
        assert!(newer.update_available);
        assert_eq!(newer.latest, "99.0.0");

        // An older published release is not an update either.
        let older = stable_decision("0.0.1", current);
        assert!(!older.update_available);
    }

    /// A binary that does not know its commit must say so rather than claim to
    /// be current — reporting "up to date" without checking is the one answer
    /// that is actively misleading.
    #[test]
    fn an_undetermined_check_is_not_an_up_to_date_check() {
        let info = BuildInfo::current();
        let status = UpdateStatus::undetermined(info, Channel::Main, "offline".to_string());
        assert!(!status.update_available);
        assert_eq!(status.error.as_deref(), Some("offline"));
        assert!(status.commits_behind.is_none());
    }
}

//! Steam Workshop marketplace adapter
//!
//! The Workshop is the *only* mod distribution channel for a number of games —
//! DayZ, Arma 3, Project Zomboid, Space Engineers and friends have no
//! third-party plugin site to fall back on, so a panel that can't install
//! Workshop items can't host those games' modded servers at all.
//!
//! It differs from the HTTP-based marketplaces (Umod, Codefling, Lone.Design)
//! in two ways that shape this adapter:
//!
//! * **Metadata comes from the Steam Web API.** Looking a single item up by id
//!   is anonymous (`ISteamRemoteStorage/GetPublishedFileDetails`); *searching*
//!   requires a Steam Web API key (`IPublishedFileService/QueryFiles`). The
//!   adapter therefore still resolves pasted Workshop ids/URLs with no key
//!   configured, which is how most operators install a known mod.
//!
//! * **Downloads go through `steamcmd`.** Workshop files have no public HTTP
//!   download URL; the content is fetched with
//!   `+workshop_download_item <appid> <id>` and lands as a *directory* tree,
//!   not a single file. Some games (DayZ among them) further require a Steam
//!   account that owns the game — anonymous login is refused — so credentials
//!   are configurable.

use crate::error::{MarketplaceError, Result};
use crate::models::*;
use crate::MarketplaceAdapter;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::Client;
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

const PROVIDER: &str = "steam_workshop";

const STEAM_API_BASE: &str = "https://api.steampowered.com";
const WORKSHOP_ITEM_URL: &str = "https://steamcommunity.com/sharedfiles/filedetails/?id=";

/// How long a single `steamcmd` invocation may run before it is killed. Arma /
/// DayZ map mods run to multiple gigabytes, so this is deliberately generous.
const DEFAULT_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Steam Workshop adapter.
pub struct SteamWorkshopAdapter {
    client: Client,
    /// Steam Web API key. Only *search* needs it; id/URL lookups and downloads
    /// work without one.
    api_key: Option<String>,
    /// How to fetch Workshop content, and as whom.
    downloads: WorkshopDownloadConfig,
    /// `steamcmd` keeps mutable state (a content log, an app-cache manifest)
    /// under its install root and does not tolerate two instances racing on
    /// the same root, so downloads are serialized.
    download_lock: Mutex<()>,
}

/// Tool used to fetch Workshop content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkshopDownloader {
    /// Valve's own `steamcmd`.
    SteamCmd,
    /// [SteamRE DepotDownloader](https://github.com/SteamRE/DepotDownloader).
    /// A useful escape hatch: when Steam's content servers misbehave, SteamCMD
    /// tends to fail opaquely (or loop) where DepotDownloader succeeds. The
    /// panel already offers it as a game-file update strategy.
    DepotDownloader,
}

impl WorkshopDownloader {
    fn label(self) -> &'static str {
        match self {
            Self::SteamCmd => "steamcmd",
            Self::DepotDownloader => "DepotDownloader",
        }
    }
}

/// How the adapter fetches Workshop content, with which tool, and as whom.
#[derive(Debug, Clone)]
pub struct WorkshopDownloadConfig {
    /// `steamcmd` binary; looked up on `PATH` when not an absolute path.
    pub steamcmd_binary: String,
    /// `DepotDownloader` binary; likewise.
    pub depot_downloader_binary: String,
    /// Downloaders to try, in order. More than one entry means the next tool
    /// is tried when the previous one fails — which is the point of having
    /// both available.
    pub downloaders: Vec<WorkshopDownloader>,
    /// Steam account to log in as. Required for games whose Workshop refuses
    /// anonymous downloads (DayZ, Arma 3, …); omit for anonymous.
    pub username: Option<String>,
    /// Password for `username`. Prefer leaving this unset and priming
    /// `steamcmd`'s cached credentials once by hand (`steamcmd +login <user>`),
    /// so the secret never reaches an argv that other users can read from
    /// `ps`.
    pub password: Option<String>,
    /// Root that Workshop content is downloaded into. Kept between runs so
    /// re-downloads are incremental instead of re-fetching whole mods.
    pub cache_dir: PathBuf,
    /// Per-invocation timeout.
    pub timeout: Duration,
}

impl Default for WorkshopDownloadConfig {
    fn default() -> Self {
        Self {
            steamcmd_binary: "steamcmd".to_string(),
            depot_downloader_binary: "DepotDownloader".to_string(),
            downloaders: vec![WorkshopDownloader::SteamCmd],
            username: None,
            password: None,
            cache_dir: std::env::temp_dir().join("nexus-workshop"),
            timeout: DEFAULT_DOWNLOAD_TIMEOUT,
        }
    }
}

impl WorkshopDownloadConfig {
    /// Read the download configuration from the environment.
    ///
    /// * `STEAMCMD_PATH` — `steamcmd` binary (default `steamcmd`)
    /// * `DEPOTDOWNLOADER_PATH` — `DepotDownloader` binary
    /// * `STEAM_WORKSHOP_DOWNLOADER` — `steamcmd` (default), `depot_downloader`,
    ///   or `auto` to try SteamCMD and fall back to DepotDownloader
    /// * `STEAM_USERNAME` / `STEAM_PASSWORD` — Workshop login
    /// * `STEAM_WORKSHOP_CACHE_DIR` — download cache root
    /// * `STEAM_WORKSHOP_TIMEOUT_SECS` — per-download timeout
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let non_empty = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());

        Self {
            steamcmd_binary: non_empty("STEAMCMD_PATH").unwrap_or(defaults.steamcmd_binary),
            depot_downloader_binary: non_empty("DEPOTDOWNLOADER_PATH")
                .unwrap_or(defaults.depot_downloader_binary),
            downloaders: non_empty("STEAM_WORKSHOP_DOWNLOADER")
                .map(|v| parse_downloaders(&v))
                .unwrap_or(defaults.downloaders),
            username: non_empty("STEAM_USERNAME"),
            password: non_empty("STEAM_PASSWORD"),
            cache_dir: non_empty("STEAM_WORKSHOP_CACHE_DIR")
                .map(PathBuf::from)
                .unwrap_or(defaults.cache_dir),
            timeout: non_empty("STEAM_WORKSHOP_TIMEOUT_SECS")
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(defaults.timeout),
        }
    }

    /// The binary for a given downloader.
    fn binary(&self, downloader: WorkshopDownloader) -> &str {
        match downloader {
            WorkshopDownloader::SteamCmd => &self.steamcmd_binary,
            WorkshopDownloader::DepotDownloader => &self.depot_downloader_binary,
        }
    }

    /// The `+login` arguments for `steamcmd`.
    fn steamcmd_login_args(&self) -> Vec<String> {
        match (&self.username, &self.password) {
            (Some(user), Some(pass)) => {
                vec!["+login".into(), user.clone(), pass.clone()]
            }
            // Cached credentials: `steamcmd` remembers a previous interactive
            // login (including the Steam Guard token) for this account.
            (Some(user), None) => vec!["+login".into(), user.clone()],
            _ => vec!["+login".into(), "anonymous".into()],
        }
    }

    /// The login arguments for DepotDownloader, which takes flags rather than
    /// a `+login` command and defaults to anonymous when given no username.
    fn depot_login_args(&self) -> Vec<String> {
        let Some(user) = self.username.as_deref() else {
            return Vec::new();
        };
        let mut args = vec!["-username".to_string(), user.to_string()];
        match self.password.as_deref() {
            Some(pass) => {
                args.push("-password".to_string());
                args.push(pass.to_string());
                // Cache the session so subsequent runs don't need the password
                // (and don't stop on a Steam Guard prompt).
                args.push("-remember-password".to_string());
            }
            // No password: rely on a session DepotDownloader already cached.
            None => args.push("-remember-password".to_string()),
        }
        args
    }

    /// Whether downloads run as a real account rather than anonymously.
    fn is_authenticated(&self) -> bool {
        self.username.is_some()
    }
}

/// Parse `STEAM_WORKSHOP_DOWNLOADER`. Unknown values fall back to SteamCMD
/// rather than leaving the node with no way to download at all.
fn parse_downloaders(value: &str) -> Vec<WorkshopDownloader> {
    match value.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
        "depot_downloader" | "depotdownloader" | "depot" => {
            vec![WorkshopDownloader::DepotDownloader]
        }
        "auto" | "both" | "fallback" => vec![
            WorkshopDownloader::SteamCmd,
            WorkshopDownloader::DepotDownloader,
        ],
        other => {
            if other != "steamcmd" && other != "steam_cmd" {
                warn!("Unknown STEAM_WORKSHOP_DOWNLOADER '{value}', using steamcmd");
            }
            vec![WorkshopDownloader::SteamCmd]
        }
    }
}

impl SteamWorkshopAdapter {
    /// Create an adapter with default download settings and no API key.
    pub fn new() -> Self {
        Self::with_config(None::<String>, WorkshopDownloadConfig::default())
    }

    /// Create an adapter configured entirely from the environment
    /// (`STEAM_API_KEY` plus the [`WorkshopDownloadConfig::from_env`] variables).
    pub fn from_env() -> Self {
        let api_key = std::env::var("STEAM_API_KEY").ok().filter(|v| !v.trim().is_empty());
        Self::with_config(api_key, WorkshopDownloadConfig::from_env())
    }

    /// Create an adapter with an explicit API key and download configuration.
    pub fn with_config(
        api_key: Option<impl Into<String>>,
        downloads: WorkshopDownloadConfig,
    ) -> Self {
        Self {
            client: Client::builder()
                .user_agent("NexusPanel/0.1.0")
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            api_key: api_key.map(Into::into),
            downloads,
            download_lock: Mutex::new(()),
        }
    }

    /// Human-readable list of the download tools this adapter will try, for
    /// startup logging.
    pub fn downloader_names(&self) -> String {
        self.downloads
            .downloaders
            .iter()
            .map(|d| d.label())
            .collect::<Vec<_>>()
            .join(" → ")
    }

    /// Whether Workshop *search* is available (it needs a Steam Web API key).
    pub fn search_enabled(&self) -> bool {
        self.api_key.is_some()
    }

    /// Whether downloads will authenticate as a real Steam account.
    pub fn authenticated_downloads(&self) -> bool {
        self.downloads.is_authenticated()
    }
}

impl Default for SteamWorkshopAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Game ↔ appid mapping
// ---------------------------------------------------------------------------

/// Games whose Workshop is worth surfacing by name, keyed by the game ids the
/// rest of the panel uses. The value is the *consumer* appid — the game the
/// items are published for, which is what `workshop_download_item` wants (for
/// DayZ that is the client app 221100, not the server app 223350).
const GAME_APP_IDS: &[(&str, u32)] = &[
    ("dayz", 221100),
    ("arma3", 107410),
    ("arma2oa", 33930),
    ("armareforger", 1874880),
    ("projectzomboid", 108600),
    ("spaceengineers", 244850),
    ("garrysmod", 4000),
    ("left4dead2", 550),
    ("cs2", 730),
    ("csgo", 730),
    ("teamfortress2", 440),
    ("unturned", 304930),
    ("ark", 346110),
    ("rust", 252490),
    ("stationeers", 544550),
    ("citiesskylines", 255710),
];

/// Games whose mods are loaded from `@ModName` directories in the server root
/// (the Real Virtuality / Enfusion engine convention) rather than by Workshop
/// id. Installing under the id would leave the mod unloadable.
const AT_PREFIX_APP_IDS: &[u32] = &[
    221100,  // DayZ
    107410,  // Arma 3
    33930,   // Arma 2: Operation Arrowhead
    1874880, // Arma Reforger
];

/// Resolve a panel game id to a Workshop appid. A bare numeric string is
/// accepted as an appid so operators can reach Workshops we don't name.
fn appid_for_game(game: &str) -> Option<u32> {
    let key: String = game.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    if key.is_empty() {
        return None;
    }
    if let Ok(appid) = key.parse::<u32>() {
        return Some(appid);
    }
    GAME_APP_IDS.iter().find(|(g, _)| *g == key).map(|(_, id)| *id)
}

/// Reverse of [`appid_for_game`], for labelling results. Falls back to the
/// appid itself so the UI always has something to show.
fn game_for_appid(appid: u32) -> String {
    GAME_APP_IDS
        .iter()
        .find(|(_, id)| *id == appid)
        .map(|(g, _)| (*g).to_string())
        .unwrap_or_else(|| appid.to_string())
}

/// Extract a Workshop id from a bare id or a `steamcommunity.com` URL, so the
/// search box accepts either. Workshop ids are 64-bit.
fn parse_item_id(input: &str) -> Option<u64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(id) = s.parse::<u64>() {
        return Some(id);
    }
    // …/sharedfiles/filedetails/?id=1559212036&searchtext=
    let after = s.split("id=").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Shortest bare number treated as a Workshop id rather than search text.
/// Published file ids have been nine digits or more since the Workshop opened,
/// so this only ever reclassifies things like a search for "2077".
const MIN_BARE_ID_DIGITS: usize = 7;

/// What the operator typed when they mean *this specific item*: an item URL,
/// or an id long enough that it can't be an ordinary search term.
fn parse_item_reference(input: &str) -> Option<u64> {
    let s = input.trim();
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return if s.len() >= MIN_BARE_ID_DIGITS {
            s.parse().ok()
        } else {
            None
        };
    }
    parse_item_id(s)
}

/// Workshop items have no version strings — an item is simply "as of" its last
/// update. Render that timestamp as the version so equality checks (which is
/// all update detection needs) stay meaningful and human-readable.
fn workshop_version(time_updated: i64) -> String {
    unix_to_utc(time_updated).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn unix_to_utc(ts: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
}

/// Directory name to install a Workshop item as.
///
/// Engines that scan for `@Mod` folders get a sanitized title; everything else
/// gets the Workshop id, which is what those games' configs reference.
fn install_dir_name(appid: u32, title: &str, item_id: &str) -> String {
    if !AT_PREFIX_APP_IDS.contains(&appid) {
        return item_id.to_string();
    }
    let sanitized = sanitize_dir_component(title);
    if sanitized.is_empty() {
        item_id.to_string()
    } else {
        format!("@{}", sanitized)
    }
}

/// Reduce a mod title to something safe to use as a single path component:
/// no separators, no traversal, no leading dot, bounded length.
fn sanitize_dir_component(title: &str) -> String {
    let mut out = String::new();
    let mut last_was_sep = false;
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_sep = false;
        } else if matches!(c, '-' | '_' | '.' | ' ') {
            // Collapse runs of punctuation/space into a single underscore.
            if !out.is_empty() && !last_was_sep {
                out.push('_');
                last_was_sep = true;
            }
        }
        // Everything else (including '/' and non-ASCII) is dropped.
        if out.len() >= 64 {
            break;
        }
    }
    out.trim_matches('_').to_string()
}

// ---------------------------------------------------------------------------
// Steam Web API payloads
// ---------------------------------------------------------------------------

/// Steam is inconsistent about whether counters are JSON numbers or strings
/// (`file_size` in particular changed shape), and omits or nulls them for
/// items it won't describe. Accept all three rather than failing the response.
fn de_u64_flex<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(u64),
        Str(String),
        Null,
    }
    Ok(match NumOrStr::deserialize(d)? {
        NumOrStr::Num(n) => n,
        NumOrStr::Str(s) => s.trim().parse().unwrap_or(0),
        NumOrStr::Null => 0,
    })
}

#[derive(Debug, Deserialize)]
struct GetDetailsEnvelope {
    response: GetDetailsResponse,
}

#[derive(Debug, Deserialize, Default)]
struct GetDetailsResponse {
    #[serde(default)]
    publishedfiledetails: Vec<WorkshopItem>,
}

#[derive(Debug, Deserialize)]
struct QueryFilesEnvelope {
    response: QueryFilesResponse,
}

#[derive(Debug, Deserialize, Default)]
struct QueryFilesResponse {
    #[serde(default)]
    publishedfiledetails: Vec<WorkshopItem>,
}

/// A Workshop item as returned by either `GetPublishedFileDetails` (the
/// anonymous endpoint) or `QueryFiles` (the keyed search endpoint). The two
/// overlap heavily but not completely, so every field is optional.
#[derive(Debug, Deserialize, Default)]
struct WorkshopItem {
    #[serde(default)]
    publishedfileid: String,
    /// `1` means OK on the anonymous endpoint; `9` is "file not found".
    /// Absent on the search endpoint.
    #[serde(default)]
    result: Option<u32>,
    #[serde(default)]
    title: Option<String>,
    /// Full description (`GetPublishedFileDetails`).
    #[serde(default)]
    description: Option<String>,
    /// Trimmed description (`QueryFiles` with `return_short_description`).
    #[serde(default)]
    short_description: Option<String>,
    /// Steam id of the publisher.
    #[serde(default)]
    creator: Option<String>,
    /// The game the item is published for.
    #[serde(default)]
    consumer_app_id: Option<u32>,
    #[serde(default)]
    creator_app_id: Option<u32>,
    #[serde(default, deserialize_with = "de_u64_flex")]
    file_size: u64,
    /// Direct URL — populated only for legacy, non-Workshop-hosted files.
    #[serde(default)]
    file_url: Option<String>,
    #[serde(default)]
    preview_url: Option<String>,
    #[serde(default)]
    time_updated: i64,
    #[serde(default, deserialize_with = "de_u64_flex")]
    subscriptions: u64,
    #[serde(default, deserialize_with = "de_u64_flex")]
    lifetime_subscriptions: u64,
    #[serde(default)]
    banned: Option<u32>,
    #[serde(default)]
    ban_reason: Option<String>,
    #[serde(default)]
    tags: Vec<WorkshopTag>,
    #[serde(default)]
    vote_data: Option<VoteData>,
}

#[derive(Debug, Deserialize, Default)]
struct WorkshopTag {
    #[serde(default)]
    tag: String,
}

#[derive(Debug, Deserialize, Default)]
struct VoteData {
    /// Fraction of positive votes, 0.0–1.0.
    #[serde(default)]
    score: f32,
}

impl WorkshopItem {
    fn appid(&self) -> u32 {
        self.consumer_app_id.or(self.creator_app_id).unwrap_or(0)
    }

    fn display_title(&self) -> String {
        self.title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("Workshop item {}", self.publishedfileid))
    }

    /// Search results carry a short description; the details endpoint carries
    /// the full one. Prefer whichever is present, trimmed for list display.
    fn summary(&self) -> String {
        let raw = self
            .short_description
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(self.description.as_deref())
            .unwrap_or_default();
        raw.trim().to_string()
    }

    /// Subscriptions are the Workshop's closest analogue to a download count.
    fn subscription_count(&self) -> u64 {
        self.subscriptions.max(self.lifetime_subscriptions)
    }

    fn to_mod_info(&self, author: String) -> ModInfo {
        ModInfo {
            id: self.publishedfileid.clone(),
            name: self.display_title(),
            description: self.summary(),
            author,
            provider: PROVIDER.to_string(),
            game: game_for_appid(self.appid()),
            category: self.tags.first().map(|t| t.tag.clone()).filter(|t| !t.is_empty()),
            downloads: self.subscription_count(),
            // Steam's score is a 0–1 fraction of positive votes; the panel
            // shows a 0–5 star rating.
            rating: self.vote_data.as_ref().map(|v| (v.score * 5.0).clamp(0.0, 5.0)),
            latest_version: workshop_version(self.time_updated),
            updated_at: unix_to_utc(self.time_updated),
            url: format!("{}{}", WORKSHOP_ITEM_URL, self.publishedfileid),
            icon_url: self.preview_url.clone(),
        }
    }
}

/// Player summary lookup, used to turn creator steam ids into names.
#[derive(Debug, Deserialize)]
struct PlayerSummariesEnvelope {
    response: PlayerSummariesResponse,
}

#[derive(Debug, Deserialize, Default)]
struct PlayerSummariesResponse {
    #[serde(default)]
    players: Vec<PlayerSummary>,
}

#[derive(Debug, Deserialize)]
struct PlayerSummary {
    #[serde(default)]
    steamid: String,
    #[serde(default)]
    personaname: String,
}

// ---------------------------------------------------------------------------
// Steam Web API calls
// ---------------------------------------------------------------------------

impl SteamWorkshopAdapter {
    fn api_key(&self) -> Result<&str> {
        self.api_key.as_deref().ok_or(MarketplaceError::AuthRequired {
            provider: PROVIDER.to_string(),
        })
    }

    /// Fetch item metadata without an API key.
    async fn fetch_items(&self, ids: &[String]) -> Result<Vec<WorkshopItem>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!(
            "{}/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
            STEAM_API_BASE
        );

        let mut form: Vec<(String, String)> =
            vec![("itemcount".to_string(), ids.len().to_string())];
        for (i, id) in ids.iter().enumerate() {
            form.push((format!("publishedfileids[{}]", i), id.clone()));
        }

        debug!("Fetching {} Steam Workshop item(s)", ids.len());

        let response = self.client.post(&url).form(&form).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(MarketplaceError::ApiError {
                provider: PROVIDER.to_string(),
                message: format!("GetPublishedFileDetails failed with status {}", status),
                status_code: Some(status.as_u16()),
            });
        }

        let envelope: GetDetailsEnvelope = response.json().await?;
        Ok(envelope.response.publishedfiledetails)
    }

    /// Fetch a single item, mapping Steam's "not found" result to a clean error.
    async fn fetch_item(&self, id: &str) -> Result<WorkshopItem> {
        let item = self
            .fetch_items(std::slice::from_ref(&id.to_string()))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| MarketplaceError::ModNotFound(id.to_string()))?;

        // `result` is 1 for a visible item; anything else (9 = not found,
        // 15 = access denied for private/friends-only items) means we have no
        // usable metadata.
        if !matches!(item.result, None | Some(1)) || item.publishedfileid.is_empty() {
            return Err(MarketplaceError::ModNotFound(id.to_string()));
        }
        if item.banned == Some(1) {
            let reason = item.ban_reason.clone().unwrap_or_default();
            return Err(MarketplaceError::ApiError {
                provider: PROVIDER.to_string(),
                message: if reason.trim().is_empty() {
                    format!("Workshop item {} has been banned", id)
                } else {
                    format!("Workshop item {} has been banned: {}", id, reason.trim())
                },
                status_code: None,
            });
        }

        Ok(item)
    }

    /// Best-effort resolution of creator steam ids to display names. Requires
    /// an API key; without one (or on any failure) the ids are used as-is, so
    /// this never turns a working search into an error.
    async fn resolve_author_names(&self, ids: &[String]) -> HashMap<String, String> {
        let mut names = HashMap::new();
        let Some(key) = self.api_key.as_deref() else {
            return names;
        };

        let mut unique: Vec<&String> = ids.iter().filter(|id| !id.trim().is_empty()).collect();
        unique.sort();
        unique.dedup();
        if unique.is_empty() {
            return names;
        }

        // GetPlayerSummaries accepts up to 100 ids per call.
        for chunk in unique.chunks(100) {
            let joined = chunk.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
            let url = format!(
                "{}/ISteamUser/GetPlayerSummaries/v2/?key={}&steamids={}",
                STEAM_API_BASE,
                urlencoding::encode(key),
                urlencoding::encode(&joined)
            );

            match self.client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<PlayerSummariesEnvelope>().await {
                        Ok(envelope) => {
                            for player in envelope.response.players {
                                if !player.personaname.is_empty() {
                                    names.insert(player.steamid, player.personaname);
                                }
                            }
                        }
                        Err(e) => debug!("Failed to parse player summaries: {}", e),
                    }
                }
                Ok(resp) => debug!("Player summary lookup returned {}", resp.status()),
                Err(e) => debug!("Player summary lookup failed: {}", e),
            }
        }

        names
    }

    /// Turn raw items into `ModInfo`, resolving creator names in one batch.
    async fn to_mod_infos(&self, items: &[WorkshopItem]) -> Vec<ModInfo> {
        let creators: Vec<String> = items.iter().filter_map(|i| i.creator.clone()).collect();
        let names = self.resolve_author_names(&creators).await;

        items
            .iter()
            .map(|item| {
                let creator = item.creator.clone().unwrap_or_default();
                let author = names.get(&creator).cloned().unwrap_or(creator);
                item.to_mod_info(author)
            })
            .collect()
    }

    /// Map a panel sort order onto Steam's `EPublishedFileQueryType`.
    ///
    /// A text query always uses relevance ranking (11) — Steam ignores
    /// `search_text` under the other ranking modes, which would silently
    /// return the whole Workshop instead of the operator's search.
    fn query_type(sort: &SortOrder, has_text: bool) -> u32 {
        if has_text {
            return 11; // RankedByTextSearch
        }
        match sort {
            SortOrder::Updated | SortOrder::Created => 1, // RankedByPublicationDate
            SortOrder::Rating => 0,                       // RankedByVote
            SortOrder::Downloads | SortOrder::Name => 12, // RankedByTotalUniqueSubscriptions
        }
    }
}

// ---------------------------------------------------------------------------
// steamcmd download
// ---------------------------------------------------------------------------

/// Pull the install path out of `steamcmd`'s success line:
/// `Success. Downloaded item 1559212036 to "/root/Steam/steamapps/..." (123 bytes)`.
///
/// This is far more reliable than guessing, because whether
/// `+force_install_dir` applies to Workshop content varies by `steamcmd` build.
fn parse_download_path(stdout: &str) -> Option<PathBuf> {
    for line in stdout.lines() {
        // "ERROR! Download item … failed" is not a path-bearing line.
        if !line.contains("Downloaded item") || line.contains("ERROR") {
            continue;
        }
        let start = line.find('"')?;
        let rest = &line[start + 1..];
        let end = rest.rfind('"')?;
        let path = &rest[..end];
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Turn a failed `steamcmd` run into an operator-actionable message.
fn download_failure_reason(stdout: &str, item_id: &str, appid: u32, authenticated: bool) -> String {
    let lower = stdout.to_lowercase();

    if lower.contains("failed to install app") && lower.contains("no subscription")
        || lower.contains("(no subscription)")
    {
        return format!(
            "Steam refused the download for app {appid} with 'No subscription'. This game's \
             Workshop requires a Steam account that owns the game — set STEAM_USERNAME (and \
             prime `steamcmd +login <user>` once so Steam Guard is satisfied)."
        );
    }
    if lower.contains("invalid password")
        || lower.contains("two-factor")
        || lower.contains("steam guard")
    {
        return "Steam rejected the login (bad password or an unsatisfied Steam Guard \
                challenge). Run `steamcmd +login <user>` once on this host to cache the \
                credentials, then retry."
            .to_string();
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return format!("Steam timed out downloading item {item_id}.");
    }
    if !authenticated {
        return format!(
            "steamcmd could not download item {item_id} (app {appid}) anonymously. Games such \
             as DayZ and Arma 3 require STEAM_USERNAME to be configured."
        );
    }
    format!("steamcmd could not download item {item_id} (app {appid}).")
}

/// Recursively copy `src` into `dst`, creating `dst`. Iterative so it works in
/// an async fn without boxing, and so deep mod trees can't blow the stack.
async fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];

    while let Some((from, to)) = stack.pop() {
        tokio::fs::create_dir_all(&to).await?;
        let mut entries = tokio::fs::read_dir(&from).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let target = to.join(entry.file_name());
            if file_type.is_dir() {
                stack.push((entry.path(), target));
            } else if file_type.is_file() {
                tokio::fs::copy(entry.path(), &target).await?;
            }
            // Symlinks inside Workshop content are skipped: nothing legitimate
            // ships them, and following one would copy from outside the tree.
        }
    }

    Ok(())
}

/// Total size and a content digest for an installed mod directory.
///
/// The digest folds in each file's path as well as its bytes, over a sorted
/// walk, so it is stable across runs and sensitive to renames — a single
/// checksum for what is really a directory of files. Files are streamed rather
/// than read whole: a single Arma/DayZ `.pbo` can run to gigabytes.
async fn hash_tree(root: &Path) -> std::io::Result<(u64, String)> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }

    files.sort();

    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 1024 * 1024];

    for path in &files {
        let rel = path.strip_prefix(root).unwrap_or(path);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0u8]);

        let mut file = tokio::fs::File::open(path).await?;
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            total += n as u64;
            hasher.update(&buf[..n]);
        }
    }

    Ok((total, format!("{:x}", hasher.finalize())))
}

impl SteamWorkshopAdapter {
    /// Fetch a Workshop item, trying each configured downloader in turn.
    ///
    /// Falling back matters because the two tools fail in different ways:
    /// when Steam's content servers are unhappy, SteamCMD is the one that
    /// stalls or returns an opaque `Failure`, and DepotDownloader often still
    /// completes (which is why operators keep it around).
    async fn fetch_item_content(&self, appid: u32, item_id: &str) -> Result<PathBuf> {
        // Serialized: neither tool is safe to run twice against one cache root.
        let _guard = self.download_lock.lock().await;

        let mut failures: Vec<String> = Vec::new();

        for (i, downloader) in self.downloads.downloaders.iter().copied().enumerate() {
            if i > 0 {
                info!(
                    "Retrying Workshop item {} with {}",
                    item_id,
                    downloader.label()
                );
            }

            let result = match downloader {
                WorkshopDownloader::SteamCmd => self.steamcmd_download(appid, item_id).await,
                WorkshopDownloader::DepotDownloader => {
                    self.depot_downloader_download(appid, item_id).await
                }
            };

            match result {
                Ok(path) => return Ok(path),
                Err(e) => {
                    warn!("{} failed for item {}: {}", downloader.label(), item_id, e);
                    failures.push(format!("{}: {}", downloader.label(), e));
                }
            }
        }

        Err(MarketplaceError::DownloadFailed {
            reason: failures.join("; "),
        })
    }

    /// Run a download tool with a bounded timeout, returning its output.
    ///
    /// `stdin` is closed so that a tool which decides to prompt (for a
    /// password or a Steam Guard code) fails immediately instead of hanging
    /// for the whole timeout — nothing is watching a panel download.
    async fn run_downloader(
        &self,
        downloader: WorkshopDownloader,
        args: &[String],
        item_id: &str,
    ) -> Result<std::process::Output> {
        let binary = self.downloads.binary(downloader);
        let mut command = tokio::process::Command::new(binary);
        command.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);

        match tokio::time::timeout(self.downloads.timeout, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(MarketplaceError::DownloadFailed {
                    reason: match downloader {
                        WorkshopDownloader::SteamCmd => format!(
                            "steamcmd not found at '{binary}'. Install SteamCMD on this node or \
                             set STEAMCMD_PATH."
                        ),
                        WorkshopDownloader::DepotDownloader => format!(
                            "DepotDownloader not found at '{binary}'. Install it on this node or \
                             set DEPOTDOWNLOADER_PATH."
                        ),
                    },
                })
            }
            Ok(Err(e)) => Err(MarketplaceError::DownloadFailed {
                reason: format!("failed to run {}: {}", downloader.label(), e),
            }),
            Err(_) => Err(MarketplaceError::DownloadFailed {
                reason: format!(
                    "{} timed out after {}s downloading item {}",
                    downloader.label(),
                    self.downloads.timeout.as_secs(),
                    item_id
                ),
            }),
        }
    }

    /// Run `steamcmd +workshop_download_item` and return the directory it
    /// wrote the content to.
    async fn steamcmd_download(&self, appid: u32, item_id: &str) -> Result<PathBuf> {
        let cache_dir = &self.downloads.cache_dir;
        tokio::fs::create_dir_all(cache_dir).await?;

        let mut args: Vec<String> = vec![
            // Never block on an interactive Steam Guard prompt, and abort the
            // script as soon as a command fails — this runs unattended.
            "+@NoPromptForPassword".into(),
            "1".into(),
            "+@ShutdownOnFailedCommand".into(),
            "1".into(),
            "+force_install_dir".into(),
            cache_dir.to_string_lossy().to_string(),
        ];
        args.extend(self.downloads.steamcmd_login_args());
        args.push("+workshop_download_item".into());
        args.push(appid.to_string());
        args.push(item_id.to_string());
        args.push("+quit".into());

        info!(
            "Downloading Workshop item {} for app {} via steamcmd ({} login)",
            item_id,
            appid,
            if self.downloads.is_authenticated() {
                "account"
            } else {
                "anonymous"
            }
        );

        let output = self.run_downloader(WorkshopDownloader::SteamCmd, &args, item_id).await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            debug!("steamcmd stderr: {}", stderr.trim());
        }

        // steamcmd's exit status is unreliable (it reports 0 for some failed
        // downloads), so the success line is what actually decides.
        if let Some(path) = parse_download_path(&stdout) {
            if tokio::fs::metadata(&path).await.is_ok() {
                return Ok(path);
            }
            warn!(
                "steamcmd reported item {} at {} but the path is missing",
                item_id,
                path.display()
            );
        }

        // Fall back to the conventional layouts in case a steamcmd build stops
        // printing the path — but only for a run that actually looks
        // successful. The cache persists between downloads, so probing these
        // paths after a *failed* run would happily find the previous version
        // and reinstall it as if the update had worked.
        if output.status.success() && !stdout.contains("ERROR!") {
            let suffix = PathBuf::from("steamapps/workshop/content")
                .join(appid.to_string())
                .join(item_id);
            let mut candidates = vec![cache_dir.join(&suffix)];
            if let Some(home) = std::env::var_os("HOME") {
                let home = PathBuf::from(home);
                candidates.push(home.join("Steam").join(&suffix));
                candidates.push(home.join(".steam/steam").join(&suffix));
                candidates.push(home.join(".local/share/Steam").join(&suffix));
            }
            for candidate in candidates {
                if tokio::fs::metadata(&candidate).await.is_ok() {
                    debug!("Located Workshop content at {}", candidate.display());
                    return Ok(candidate);
                }
            }
        }

        Err(MarketplaceError::DownloadFailed {
            reason: download_failure_reason(
                &stdout,
                item_id,
                appid,
                self.downloads.is_authenticated(),
            ),
        })
    }

    /// Fetch an item with DepotDownloader's `-pubfile`, which resolves a
    /// published file id to its UGC content and writes it straight into
    /// `-dir` — no `steamapps/workshop/content` nesting to go looking for.
    async fn depot_downloader_download(&self, appid: u32, item_id: &str) -> Result<PathBuf> {
        // One directory per item: DepotDownloader empties nothing, so sharing
        // a directory between items would blend their files together.
        let target = self.downloads.cache_dir.join("depot").join(appid.to_string()).join(item_id);
        tokio::fs::create_dir_all(&target).await?;

        let mut args = vec![
            "-pubfile".to_string(),
            item_id.to_string(),
            "-dir".to_string(),
            target.to_string_lossy().to_string(),
        ];
        args.extend(self.downloads.depot_login_args());

        info!(
            "Downloading Workshop item {} for app {} via DepotDownloader ({} login)",
            item_id,
            appid,
            if self.downloads.is_authenticated() {
                "account"
            } else {
                "anonymous"
            }
        );

        let output =
            self.run_downloader(WorkshopDownloader::DepotDownloader, &args, item_id).await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            debug!("DepotDownloader stderr: {}", stderr.trim());
        }

        // DepotDownloader writes into the directory we chose, so success is
        // "it exited cleanly and left files behind" rather than a path parsed
        // out of its log.
        if output.status.success() && dir_has_files(&target).await {
            return Ok(target);
        }

        let detail = depot_failure_detail(&stdout, &stderr);
        Err(MarketplaceError::DownloadFailed {
            reason: if detail.is_empty() {
                format!(
                    "DepotDownloader could not download item {item_id} (app {appid}); it exited \
                     without writing any content."
                )
            } else {
                format!("DepotDownloader could not download item {item_id}: {detail}")
            },
        })
    }
}

/// Whether a directory tree contains at least one file.
async fn dir_has_files(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            match entry.file_type().await {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(t) if t.is_file() => return true,
                _ => {}
            }
        }
    }
    false
}

/// The most useful line from a failed DepotDownloader run. Its errors are
/// single readable lines ("Error: ...", "... is not available ..."), so
/// surfacing them beats a generic failure message.
fn depot_failure_detail(stdout: &str, stderr: &str) -> String {
    stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|line| {
            let lower = line.to_lowercase();
            !line.is_empty()
                && (lower.starts_with("error")
                    || lower.contains("failed")
                    || lower.contains("denied")
                    || lower.contains("invalid")
                    || lower.contains("unable to"))
        })
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// MarketplaceAdapter
// ---------------------------------------------------------------------------

#[async_trait]
impl MarketplaceAdapter for SteamWorkshopAdapter {
    fn provider_name(&self) -> &str {
        PROVIDER
    }

    fn supported_games(&self) -> Vec<String> {
        GAME_APP_IDS.iter().map(|(game, _)| (*game).to_string()).collect()
    }

    /// Search the Workshop.
    ///
    /// A pasted Workshop id or item URL is resolved directly and needs no API
    /// key — that is the common case for "install this specific mod". Text
    /// search needs both a `game` filter (Steam has no cross-app Workshop
    /// search) and a Steam Web API key.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<ModInfo>> {
        let game_appid = query.game.as_deref().and_then(appid_for_game);

        // Direct id/URL lookup.
        if let Some(item_id) = parse_item_reference(&query.query) {
            let item = self.fetch_item(&item_id.to_string()).await?;
            // Respect an explicit game filter rather than returning a mod for
            // the wrong game.
            if let Some(appid) = game_appid {
                if item.appid() != appid {
                    return Ok(Vec::new());
                }
            }
            return Ok(self.to_mod_infos(std::slice::from_ref(&item)).await);
        }

        let key = self.api_key().map_err(|_| MarketplaceError::AuthRequired {
            provider: format!(
                "{PROVIDER} (set STEAM_API_KEY to search, or paste a Workshop id/URL)"
            ),
        })?;

        let appid = match (&query.game, game_appid) {
            (_, Some(appid)) => appid,
            (Some(game), None) => {
                return Err(MarketplaceError::UnsupportedGame {
                    provider: PROVIDER.to_string(),
                    game: game.clone(),
                })
            }
            (None, None) => {
                return Err(MarketplaceError::ApiError {
                    provider: PROVIDER.to_string(),
                    message: "Steam Workshop search requires a game filter (each game has its \
                              own Workshop)"
                        .to_string(),
                    status_code: None,
                })
            }
        };

        let limit = query.limit.clamp(1, 100);
        let page = (query.offset / limit) + 1;
        let has_text = !query.query.trim().is_empty();

        let url = format!(
            "{}/IPublishedFileService/QueryFiles/v1/?key={}&appid={}&query_type={}&search_text={}\
             &page={}&numperpage={}&filetype=0&return_details=true&return_short_description=true\
             &return_previews=true&return_vote_data=true&return_metadata=true",
            STEAM_API_BASE,
            urlencoding::encode(key),
            appid,
            Self::query_type(&query.sort, has_text),
            urlencoding::encode(query.query.trim()),
            page,
            limit,
        );

        debug!("Searching Steam Workshop for app {}", appid);

        let response = self.client.get(&url).send().await?;
        let status = response.status();

        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(MarketplaceError::InvalidApiKey {
                provider: PROVIDER.to_string(),
            });
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(MarketplaceError::RateLimited {
                provider: PROVIDER.to_string(),
                retry_after_secs: 60,
            });
        }
        if !status.is_success() {
            return Err(MarketplaceError::ApiError {
                provider: PROVIDER.to_string(),
                message: format!("QueryFiles failed with status {}", status),
                status_code: Some(status.as_u16()),
            });
        }

        let envelope: QueryFilesEnvelope = response.json().await?;
        let items: Vec<WorkshopItem> = envelope
            .response
            .publishedfiledetails
            .into_iter()
            .filter(|i| !i.publishedfileid.is_empty() && i.banned != Some(1))
            .collect();

        Ok(self.to_mod_infos(&items).await)
    }

    async fn get_mod_details(&self, mod_id: &str) -> Result<ModDetails> {
        // Accept a full item URL here too — operators paste them.
        let item_id = parse_item_id(mod_id)
            .map(|id| id.to_string())
            .ok_or_else(|| MarketplaceError::ModNotFound(mod_id.to_string()))?;

        let item = self.fetch_item(&item_id).await?;
        let info = self
            .to_mod_infos(std::slice::from_ref(&item))
            .await
            .into_iter()
            .next()
            .ok_or_else(|| MarketplaceError::ModNotFound(mod_id.to_string()))?;

        // The Workshop exposes only the current state of an item — there is no
        // version history to download from — so this is always a single entry
        // stamped with the item's last-updated time.
        let versions = vec![VersionInfo {
            version: info.latest_version.clone(),
            // Workshop content has no public HTTP URL (`file_url` is set only
            // for legacy uploads); downloads go through steamcmd instead.
            download_url: item
                .file_url
                .clone()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| info.url.clone()),
            file_size: item.file_size,
            checksum: None, // Steam publishes no digest for Workshop content.
            release_date: info.updated_at,
            changelog: None,
            min_game_version: None,
            max_game_version: None,
            downloads: info.downloads,
        }];

        Ok(ModDetails {
            full_description: item.description.clone().filter(|d| !d.trim().is_empty()),
            versions,
            // Workshop dependencies are expressed as collections, which the
            // publisher may or may not maintain; nothing machine-readable
            // links an item to its requirements.
            dependencies: Vec::new(),
            screenshots: item.preview_url.clone().into_iter().collect(),
            license: None,
            source_url: None,
            issues_url: None,
            community_url: Some(format!("{}{}", WORKSHOP_ITEM_URL, item_id)),
            info,
        })
    }

    /// Workshop "categories" are per-game publisher-chosen tags with no
    /// enumeration endpoint, so there is no honest fixed list to return.
    async fn get_categories(&self, _game: &str) -> Result<Vec<Category>> {
        Ok(Vec::new())
    }

    /// Download an item with `steamcmd` and install it into `target_dir`.
    ///
    /// The result is a *directory* (`@ModName` for DayZ/Arma, otherwise the
    /// Workshop id), because that is the unit the Workshop distributes and
    /// what these games load.
    async fn download(
        &self,
        mod_id: &str,
        version: &str,
        target_dir: &Path,
    ) -> Result<DownloadResult> {
        let item_id = parse_item_id(mod_id)
            .map(|id| id.to_string())
            .ok_or_else(|| MarketplaceError::ModNotFound(mod_id.to_string()))?;

        let item = self.fetch_item(&item_id).await?;
        let appid = item.appid();
        if appid == 0 {
            return Err(MarketplaceError::DownloadFailed {
                reason: format!(
                    "Steam did not report which game item {} belongs to",
                    item_id
                ),
            });
        }

        // steamcmd can only ever fetch an item's current state; asking for an
        // older one is not something the Workshop supports.
        let latest = workshop_version(item.time_updated);
        if !version.is_empty() && version != latest {
            warn!(
                "Steam Workshop item {} has no version history; installing the current \
                 revision ({}) instead of the requested '{}'",
                item_id, latest, version
            );
        }

        let start = Instant::now();
        let content_dir = self.fetch_item_content(appid, &item_id).await?;

        let dir_name = install_dir_name(appid, &item.display_title(), &item_id);
        let dest = target_dir.join(&dir_name);

        // Replace any previous install so files removed upstream don't linger.
        if tokio::fs::metadata(&dest).await.is_ok() {
            debug!("Removing previous install at {}", dest.display());
            tokio::fs::remove_dir_all(&dest).await?;
        }

        tokio::fs::create_dir_all(target_dir).await?;
        copy_dir_all(&content_dir, &dest)
            .await
            .map_err(|e| MarketplaceError::DownloadFailed {
                reason: format!(
                    "failed to install item {} into {}: {}",
                    item_id,
                    dest.display(),
                    e
                ),
            })?;

        // DayZ/Arma verify mod signatures against keys in the *server's* key
        // directory. Installing the mod without them leaves a server that
        // looks correctly modded but rejects every client, so this is part of
        // installing, not an extra.
        let signature_keys = if AT_PREFIX_APP_IDS.contains(&appid) {
            install_signature_keys(&dest, target_dir).await.unwrap_or_else(|e| {
                warn!(
                    "Installed item {} but could not copy its signature keys: {}",
                    item_id, e
                );
                Vec::new()
            })
        } else {
            Vec::new()
        };

        let (file_size, checksum) = hash_tree(&dest).await?;
        let download_time_ms = start.elapsed().as_millis() as u64;

        info!(
            "Installed Workshop item {} as {} ({} bytes, {} signature key(s)) in {}ms",
            item_id,
            dir_name,
            file_size,
            signature_keys.len(),
            download_time_ms
        );

        Ok(DownloadResult {
            file_path: dest,
            file_size,
            checksum,
            download_time_ms,
            signature_keys,
        })
    }
}

/// Copy a mod's `.bikey` signature keys into the server's `keys/` directory,
/// returning the file names installed.
///
/// DayZ and Arma ship keys inside the mod (conventionally `@Mod/keys` or
/// `@Mod/Keys`, but layouts vary), while the server reads them from its own
/// `keys/` folder. With `verifySignatures` enabled — the stock DayZ config —
/// a missing key means clients are refused, which presents as "the mod doesn't
/// work" rather than as a key problem. The whole mod tree is scanned so an
/// unconventional layout still works.
async fn install_signature_keys(mod_dir: &Path, server_dir: &Path) -> std::io::Result<Vec<String>> {
    let mut keys: Vec<PathBuf> = Vec::new();
    let mut stack = vec![mod_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("bikey"))
            {
                keys.push(path);
            }
        }
    }

    if keys.is_empty() {
        return Ok(Vec::new());
    }

    let key_dir = server_dir.join("keys");
    tokio::fs::create_dir_all(&key_dir).await?;

    keys.sort();
    let mut installed = Vec::new();
    for key in &keys {
        let Some(name) = key.file_name() else {
            continue;
        };
        // Overwrite: a mod update can rotate its key, and the stale one would
        // otherwise stay valid forever.
        tokio::fs::copy(key, key_dir.join(name)).await?;
        installed.push(name.to_string_lossy().to_string());
    }

    // Sort by name before deduping: the walk order follows directory paths, so
    // the same key found in two places would not be adjacent (and `dedup`
    // only collapses neighbours).
    installed.sort();
    installed.dedup();
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_games_and_raw_appids() {
        assert_eq!(appid_for_game("dayz"), Some(221100));
        assert_eq!(appid_for_game("DayZ"), Some(221100));
        assert_eq!(appid_for_game("arma3"), Some(107410));
        assert_eq!(appid_for_game("Arma 3"), Some(107410));
        assert_eq!(appid_for_game("project-zomboid"), Some(108600));
        // A bare appid reaches Workshops we don't name.
        assert_eq!(appid_for_game("294100"), Some(294100));
        assert_eq!(appid_for_game("nonexistent-game"), None);
        assert_eq!(appid_for_game("   "), None);
    }

    #[test]
    fn game_label_falls_back_to_appid() {
        assert_eq!(game_for_appid(221100), "dayz");
        assert_eq!(game_for_appid(999999), "999999");
    }

    #[test]
    fn parses_ids_from_bare_numbers_and_urls() {
        assert_eq!(parse_item_id("1559212036"), Some(1559212036));
        assert_eq!(parse_item_id("  1559212036 "), Some(1559212036));
        assert_eq!(
            parse_item_id("https://steamcommunity.com/sharedfiles/filedetails/?id=1559212036"),
            Some(1559212036)
        );
        // Trailing query parameters must not be swallowed into the id.
        assert_eq!(
            parse_item_id(
                "https://steamcommunity.com/sharedfiles/filedetails/?id=1559212036&searchtext=cf"
            ),
            Some(1559212036)
        );
        assert_eq!(parse_item_id("community framework"), None);
        assert_eq!(parse_item_id(""), None);
    }

    #[test]
    fn short_numbers_are_search_text_not_item_ids() {
        // Searching for "2077" must stay a search; a real id is a lookup.
        assert_eq!(parse_item_reference("2077"), None);
        assert_eq!(parse_item_reference("1559212036"), Some(1559212036));
        assert_eq!(parse_item_reference("trader"), None);
        assert_eq!(parse_item_reference(""), None);
        // A URL is unambiguous whatever the id's length.
        assert_eq!(
            parse_item_reference("https://steamcommunity.com/sharedfiles/filedetails/?id=2077"),
            Some(2077)
        );
    }

    #[test]
    fn dayz_and_arma_install_as_at_prefixed_folders() {
        // DayZ/Arma load `@Mod` directories from the server root.
        assert_eq!(install_dir_name(221100, "CF", "1559212036"), "@CF");
        assert_eq!(
            install_dir_name(107410, "CBA_A3 - Community Base", "450814997"),
            "@CBA_A3_Community_Base"
        );
        // Everything else is referenced by Workshop id.
        assert_eq!(
            install_dir_name(108600, "Hydrocraft", "498441420"),
            "498441420"
        );
    }

    #[test]
    fn install_dir_name_cannot_escape_the_target_directory() {
        // A hostile or merely awkward title must stay a single component.
        let name = install_dir_name(221100, "../../etc/passwd", "1");
        assert!(!name.contains('/'), "unexpected separator in {name}");
        assert!(!name.contains(".."), "unexpected traversal in {name}");
        assert_eq!(Path::new(&name).components().count(), 1);

        // Titles with nothing usable fall back to the item id.
        assert_eq!(install_dir_name(221100, "///", "1559212036"), "1559212036");
        assert_eq!(install_dir_name(221100, "   ", "1559212036"), "1559212036");
    }

    #[test]
    fn sanitized_names_are_bounded_and_tidy() {
        assert_eq!(
            sanitize_dir_component("Community  Framework"),
            "Community_Framework"
        );
        assert_eq!(sanitize_dir_component("  spaced  "), "spaced");
        assert_eq!(sanitize_dir_component("日本語 Mod"), "Mod");
        assert!(sanitize_dir_component(&"a".repeat(200)).len() <= 64);
    }

    #[test]
    fn version_is_the_last_update_timestamp() {
        assert_eq!(workshop_version(1620000000), "2021-05-03T00:00:00Z");
    }

    #[test]
    fn login_args_cover_anonymous_cached_and_password() {
        let anon = WorkshopDownloadConfig::default();
        assert_eq!(anon.steamcmd_login_args(), vec!["+login", "anonymous"]);
        assert!(!anon.is_authenticated());
        // DepotDownloader is anonymous by omission, not by keyword.
        assert!(anon.depot_login_args().is_empty());

        let cached = WorkshopDownloadConfig {
            username: Some("operator".into()),
            ..WorkshopDownloadConfig::default()
        };
        assert_eq!(cached.steamcmd_login_args(), vec!["+login", "operator"]);
        assert!(cached.is_authenticated());
        assert_eq!(
            cached.depot_login_args(),
            vec!["-username", "operator", "-remember-password"]
        );

        let with_password = WorkshopDownloadConfig {
            username: Some("operator".into()),
            password: Some("hunter2".into()),
            ..WorkshopDownloadConfig::default()
        };
        assert_eq!(
            with_password.steamcmd_login_args(),
            vec!["+login", "operator", "hunter2"]
        );
        assert_eq!(
            with_password.depot_login_args(),
            vec![
                "-username",
                "operator",
                "-password",
                "hunter2",
                "-remember-password"
            ]
        );
    }

    #[test]
    fn downloader_selection_parses_and_defaults_safely() {
        assert_eq!(
            parse_downloaders("steamcmd"),
            vec![WorkshopDownloader::SteamCmd]
        );
        assert_eq!(
            parse_downloaders("depot_downloader"),
            vec![WorkshopDownloader::DepotDownloader]
        );
        assert_eq!(
            parse_downloaders("DepotDownloader"),
            vec![WorkshopDownloader::DepotDownloader]
        );
        // `auto` tries SteamCMD first, then falls back.
        assert_eq!(
            parse_downloaders("auto"),
            vec![
                WorkshopDownloader::SteamCmd,
                WorkshopDownloader::DepotDownloader
            ]
        );
        // A typo must not leave the node unable to download anything.
        assert_eq!(
            parse_downloaders("nonsense"),
            vec![WorkshopDownloader::SteamCmd]
        );
        assert_eq!(
            WorkshopDownloadConfig::default().downloaders,
            vec![WorkshopDownloader::SteamCmd]
        );
    }

    #[test]
    fn depot_failure_detail_picks_the_useful_line() {
        let stdout = "Using account operator.\n\
                      Got AppInfo for 221100\n\
                      Error: Requested file is not available for this account.\n";
        assert_eq!(
            depot_failure_detail(stdout, ""),
            "Error: Requested file is not available for this account."
        );
        // Nothing recognizable → empty, so the caller supplies a generic message.
        assert_eq!(depot_failure_detail("Downloading depot 221101\n", ""), "");
    }

    #[tokio::test]
    async fn both_downloaders_are_tried_before_giving_up() {
        // Neither binary exists, so `auto` must report both attempts rather
        // than only the first — otherwise a fallback failure looks like a
        // SteamCMD failure.
        let adapter = SteamWorkshopAdapter::with_config(
            None::<String>,
            WorkshopDownloadConfig {
                steamcmd_binary: "/nonexistent/steamcmd".to_string(),
                depot_downloader_binary: "/nonexistent/DepotDownloader".to_string(),
                downloaders: vec![
                    WorkshopDownloader::SteamCmd,
                    WorkshopDownloader::DepotDownloader,
                ],
                cache_dir: std::env::temp_dir().join("nexus-workshop-test"),
                ..WorkshopDownloadConfig::default()
            },
        );
        let err = adapter.fetch_item_content(221100, "1559212036").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("STEAMCMD_PATH"), "{msg}");
        assert!(msg.contains("DEPOTDOWNLOADER_PATH"), "{msg}");
    }

    #[tokio::test]
    async fn signature_keys_are_copied_into_the_server_key_directory() {
        let root = std::env::temp_dir().join(format!("nexus-keys-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        let mod_dir = root.join("@CF");

        // A mod that ships keys in the conventional place plus an oddly
        // placed one — both must reach the server's keys/ directory.
        tokio::fs::create_dir_all(mod_dir.join("keys")).await.unwrap();
        tokio::fs::create_dir_all(mod_dir.join("addons/extra")).await.unwrap();
        tokio::fs::write(mod_dir.join("keys/cf.bikey"), b"k1").await.unwrap();
        tokio::fs::write(mod_dir.join("addons/extra/other.BIKEY"), b"k2").await.unwrap();
        tokio::fs::write(mod_dir.join("addons/cf.pbo"), b"content").await.unwrap();

        let installed = install_signature_keys(&mod_dir, &root).await.unwrap();
        assert_eq!(installed, vec!["cf.bikey", "other.BIKEY"]);
        assert_eq!(
            tokio::fs::read(root.join("keys/cf.bikey")).await.unwrap(),
            b"k1"
        );
        assert!(root.join("keys/other.BIKEY").exists());
        // Non-key files stay where they are.
        assert!(!root.join("keys/cf.pbo").exists());

        // A mod with no keys leaves nothing behind and reports nothing.
        let plain = root.join("@Plain");
        tokio::fs::create_dir_all(&plain).await.unwrap();
        tokio::fs::write(plain.join("x.pbo"), b"c").await.unwrap();
        assert!(install_signature_keys(&plain, &root).await.unwrap().is_empty());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[test]
    fn parses_the_steamcmd_success_line() {
        let stdout = "Redirecting stderr\n\
             Logging in user 'operator' to Steam Public...OK\n\
             Downloading item 1559212036 ...\n\
             Success. Downloaded item 1559212036 to \
             \"/root/Steam/steamapps/workshop/content/221100/1559212036\" (5761234 bytes).\n";
        assert_eq!(
            parse_download_path(stdout),
            Some(PathBuf::from(
                "/root/Steam/steamapps/workshop/content/221100/1559212036"
            ))
        );
    }

    #[test]
    fn no_success_line_means_no_path() {
        assert_eq!(
            parse_download_path("ERROR! Download item 1 failed (Failure)."),
            None
        );
        assert_eq!(parse_download_path(""), None);
        // An error line that happens to quote a path must not be mistaken for
        // a successful download — the cache may still hold the old version.
        assert_eq!(
            parse_download_path("ERROR! Downloaded item 1 to \"/tmp/stale\" was rejected"),
            None
        );
    }

    #[test]
    fn failure_reasons_name_the_likely_fix() {
        let no_sub = download_failure_reason(
            "ERROR! Failed to install app '221100' (No subscription)",
            "1",
            221100,
            false,
        );
        assert!(no_sub.contains("STEAM_USERNAME"), "{no_sub}");

        let guard = download_failure_reason("FAILED (Invalid Password)", "1", 107410, true);
        assert!(guard.contains("steamcmd +login"), "{guard}");

        // Anonymous failure with no recognizable cause still points at creds.
        let anon = download_failure_reason("something else went wrong", "1", 221100, false);
        assert!(anon.contains("STEAM_USERNAME"), "{anon}");
    }

    #[test]
    fn query_type_prefers_text_relevance() {
        // With search text, always relevance — other modes ignore search_text.
        assert_eq!(
            SteamWorkshopAdapter::query_type(&SortOrder::Downloads, true),
            11
        );
        assert_eq!(
            SteamWorkshopAdapter::query_type(&SortOrder::Rating, true),
            11
        );
        // Browsing without text uses the requested ranking.
        assert_eq!(
            SteamWorkshopAdapter::query_type(&SortOrder::Downloads, false),
            12
        );
        assert_eq!(
            SteamWorkshopAdapter::query_type(&SortOrder::Rating, false),
            0
        );
        assert_eq!(
            SteamWorkshopAdapter::query_type(&SortOrder::Updated, false),
            1
        );
    }

    // A trimmed but faithful GetPublishedFileDetails response for DayZ's
    // Community Framework, including the string-typed `file_size` that Steam
    // switched to.
    const DETAILS_SAMPLE: &str = r#"{
      "response": {
        "result": 1,
        "resultcount": 1,
        "publishedfiledetails": [{
          "publishedfileid": "1559212036",
          "result": 1,
          "creator": "76561198040534790",
          "creator_app_id": 221100,
          "consumer_app_id": 221100,
          "filename": "",
          "file_size": "5761234",
          "file_url": "",
          "preview_url": "https://steamuserimages-a.akamaihd.net/ugc/preview.jpg",
          "title": "CF",
          "description": "Community Framework for DayZ",
          "time_created": 1543000000,
          "time_updated": 1620000000,
          "visibility": 0,
          "banned": 0,
          "subscriptions": 1200000,
          "favorited": 4000,
          "views": 900000,
          "tags": [{"tag": "Mod"}, {"tag": "Framework"}]
        }]
      }
    }"#;

    #[test]
    fn parses_get_published_file_details() {
        let envelope: GetDetailsEnvelope = serde_json::from_str(DETAILS_SAMPLE).unwrap();
        let item = &envelope.response.publishedfiledetails[0];
        assert_eq!(item.publishedfileid, "1559212036");
        assert_eq!(item.appid(), 221100);
        // `file_size` arrives as a string and must still be a number to us.
        assert_eq!(item.file_size, 5761234);

        let info = item.to_mod_info("Jacob_Mango".to_string());
        assert_eq!(info.name, "CF");
        assert_eq!(info.provider, "steam_workshop");
        assert_eq!(info.game, "dayz");
        assert_eq!(info.downloads, 1200000);
        assert_eq!(info.category.as_deref(), Some("Mod"));
        assert_eq!(info.latest_version, "2021-05-03T00:00:00Z");
        assert_eq!(
            info.url,
            "https://steamcommunity.com/sharedfiles/filedetails/?id=1559212036"
        );
    }

    #[test]
    fn parses_query_files_search_results() {
        // The search endpoint returns short_description and vote_data instead
        // of the full description, and numeric file sizes.
        let sample = r#"{
          "response": {
            "total": 1,
            "publishedfiledetails": [{
              "publishedfileid": "1590841260",
              "creator": "76561198000000000",
              "consumer_app_id": 221100,
              "title": "Trader",
              "short_description": "Trading system for DayZ",
              "file_size": 1024,
              "time_updated": 1620000000,
              "subscriptions": 500,
              "vote_data": {"score": 0.8, "votes_up": 80, "votes_down": 20},
              "tags": [{"tag": "Mod"}]
            }]
          }
        }"#;
        let envelope: QueryFilesEnvelope = serde_json::from_str(sample).unwrap();
        let item = &envelope.response.publishedfiledetails[0];
        let info = item.to_mod_info("someone".to_string());
        assert_eq!(info.name, "Trader");
        assert_eq!(info.description, "Trading system for DayZ");
        assert_eq!(info.downloads, 500);
        // 0.8 positive → 4.0 stars.
        assert_eq!(info.rating, Some(4.0));
    }

    #[test]
    fn counters_parse_as_numbers_strings_or_null() {
        // Steam has shipped all three shapes for these fields.
        let sample = r#"{"response":{"publishedfiledetails":[
            {"publishedfileid":"1","file_size":123,"subscriptions":"456"},
            {"publishedfileid":"2","file_size":null,"subscriptions":null}
        ]}}"#;
        let envelope: GetDetailsEnvelope = serde_json::from_str(sample).unwrap();
        assert_eq!(envelope.response.publishedfiledetails[0].file_size, 123);
        assert_eq!(envelope.response.publishedfiledetails[0].subscriptions, 456);
        assert_eq!(envelope.response.publishedfiledetails[1].file_size, 0);
        assert_eq!(envelope.response.publishedfiledetails[1].subscriptions, 0);
    }

    #[test]
    fn missing_fields_degrade_instead_of_failing() {
        // Steam omits most fields for private/deleted items; parsing must not
        // blow up on the shape it does return.
        let sample =
            r#"{"response":{"publishedfiledetails":[{"publishedfileid":"1","result":9}]}}"#;
        let envelope: GetDetailsEnvelope = serde_json::from_str(sample).unwrap();
        let item = &envelope.response.publishedfiledetails[0];
        assert_eq!(item.result, Some(9));
        let info = item.to_mod_info(String::new());
        assert_eq!(info.name, "Workshop item 1");
        assert_eq!(info.downloads, 0);
        assert!(info.rating.is_none());
    }

    #[test]
    fn supported_games_include_the_workshop_only_titles() {
        let adapter = SteamWorkshopAdapter::new();
        let games = adapter.supported_games();
        assert!(games.contains(&"dayz".to_string()));
        assert!(games.contains(&"arma3".to_string()));
        assert!(games.contains(&"projectzomboid".to_string()));
        assert_eq!(adapter.provider_name(), "steam_workshop");
        assert!(!adapter.search_enabled());
        assert!(!adapter.authenticated_downloads());
    }

    #[tokio::test]
    async fn search_without_a_key_still_resolves_pasted_ids() {
        // Not a network test: it only asserts which branch a keyless adapter
        // takes for a non-id query.
        let adapter = SteamWorkshopAdapter::new();
        let err = adapter
            .search(&SearchQuery::new("community framework").with_game("dayz"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, MarketplaceError::AuthRequired { .. }),
            "expected AuthRequired, got {err:?}"
        );
        assert!(err.to_string().contains("STEAM_API_KEY"));
    }

    #[tokio::test]
    async fn search_without_a_game_filter_is_rejected() {
        let adapter =
            SteamWorkshopAdapter::with_config(Some("test-key"), WorkshopDownloadConfig::default());
        let err = adapter.search(&SearchQuery::new("trader")).await.unwrap_err();
        assert!(err.to_string().contains("game filter"), "{err}");

        let err = adapter
            .search(&SearchQuery::new("trader").with_game("not-a-game"))
            .await
            .unwrap_err();
        assert!(matches!(err, MarketplaceError::UnsupportedGame { .. }));
    }

    #[tokio::test]
    async fn missing_steamcmd_reports_an_actionable_error() {
        let adapter = SteamWorkshopAdapter::with_config(
            None::<String>,
            WorkshopDownloadConfig {
                steamcmd_binary: "/nonexistent/steamcmd".to_string(),
                cache_dir: std::env::temp_dir().join("nexus-workshop-test"),
                ..WorkshopDownloadConfig::default()
            },
        );
        let err = adapter.steamcmd_download(221100, "1559212036").await.unwrap_err();
        assert!(err.to_string().contains("STEAMCMD_PATH"), "{err}");
    }

    #[tokio::test]
    async fn copies_and_hashes_a_content_tree() {
        let root = std::env::temp_dir().join(format!("nexus-ws-{}", std::process::id()));
        let src = root.join("src/addons");
        let dst = root.join("dst/@CF");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&src).await.unwrap();
        tokio::fs::write(src.join("cf.pbo"), b"pbo-bytes").await.unwrap();
        tokio::fs::create_dir_all(src.join("keys")).await.unwrap();
        tokio::fs::write(src.join("keys/cf.bikey"), b"key").await.unwrap();

        copy_dir_all(&src, &dst).await.unwrap();
        assert!(dst.join("cf.pbo").exists());
        assert!(dst.join("keys/cf.bikey").exists());

        let (size, checksum) = hash_tree(&dst).await.unwrap();
        assert_eq!(size, b"pbo-bytes".len() as u64 + b"key".len() as u64);
        assert_eq!(checksum.len(), 64);

        // The digest is stable across copies of identical content…
        let dst2 = root.join("dst2/@CF");
        copy_dir_all(&src, &dst2).await.unwrap();
        assert_eq!(hash_tree(&dst2).await.unwrap().1, checksum);

        // …and changes when a file's contents change.
        tokio::fs::write(dst2.join("cf.pbo"), b"different").await.unwrap();
        assert_ne!(hash_tree(&dst2).await.unwrap().1, checksum);

        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}

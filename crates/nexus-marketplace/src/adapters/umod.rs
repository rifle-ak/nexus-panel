//! Umod (formerly Oxide) marketplace adapter
//!
//! Umod is the primary mod marketplace for games like Rust, 7 Days to Die,
//! ARK, and other Unity/Unreal games.

use crate::error::{MarketplaceError, Result};
use crate::models::*;
use crate::MarketplaceAdapter;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use reqwest::Client;
use serde::Deserialize;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

const UMOD_API_BASE: &str = "https://umod.org/plugins";
const UMOD_SEARCH_API: &str = "https://umod.org/plugins/search.json";

/// Umod marketplace adapter
pub struct UmodAdapter {
    client: Client,
}

impl UmodAdapter {
    /// Create a new Umod adapter
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent("NexusPanel/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Map Umod game slugs to internal game IDs
    fn game_to_slug(game: &str) -> Option<&'static str> {
        match game.to_lowercase().as_str() {
            "rust" => Some("rust"),
            "7daystodie" | "7dtd" => Some("7-days-to-die"),
            "ark" | "arkse" => Some("ark-survival-evolved"),
            "conanexiles" => Some("conan-exiles"),
            "hurtworld" => Some("hurtworld"),
            "theforest" => Some("the-forest"),
            "unturned" => Some("unturned"),
            "reign" | "reignofkings" => Some("reign-of-kings"),
            "terraria" => Some("terraria"),
            "valheim" => Some("valheim"),
            _ => None,
        }
    }
}

impl Default for UmodAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Umod API search response.
///
/// The wrapper carries pagination metadata (`current_page`, `last_page`, …)
/// that we don't use; only `data` matters.
#[derive(Debug, Deserialize)]
struct UmodSearchResponse {
    #[serde(default)]
    data: Vec<UmodPlugin>,
}

/// A game reference in the Umod API (`games_detail[]`).
#[derive(Debug, Deserialize)]
struct UmodGame {
    #[serde(default)]
    slug: String,
}

/// Umod plugin data as returned by the search + details endpoints.
///
/// The Umod API has drifted over time (the display name moved from `name` to
/// `title`, `downloads_total` became `downloads`, `category` became
/// `category_tags`, `games` became `games_detail`, and timestamps gained
/// RFC3339 `*_atom` companions). Every field here is optional/defaulted so a
/// future minor drift degrades gracefully instead of failing the whole
/// response.
#[derive(Debug, Deserialize)]
struct UmodPlugin {
    #[serde(default)]
    slug: String,
    /// Human-readable display name (current API).
    #[serde(default)]
    title: Option<String>,
    /// Code/class name; also the historical display-name field.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<String>,
    /// Games this plugin targets (current API).
    #[serde(default)]
    games_detail: Vec<UmodGame>,
    /// Total downloads (current API); historically `downloads_total`.
    #[serde(default, alias = "downloads_total")]
    downloads: u64,
    #[serde(default)]
    latest_release_version: Option<String>,
    /// RFC3339 release timestamp (current API).
    #[serde(default)]
    latest_release_at_atom: Option<String>,
    /// Legacy space-separated release timestamp ("YYYY-MM-DD HH:MM:SS").
    #[serde(default)]
    latest_release_at: Option<String>,
    /// Comma-separated category tags (search endpoint). Note the details
    /// endpoint instead returns `category` as an integer id, which we ignore.
    #[serde(default)]
    category_tags: Option<String>,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    watchers: u64,
    /// Direct plugin (`.cs`) download URL, present on both endpoints.
    #[serde(default)]
    download_url: Option<String>,
    /// SHA-1 checksum of the latest release file.
    #[serde(default)]
    latest_release_version_checksum: Option<String>,
    /// Markdown long-description (details endpoint only).
    #[serde(default)]
    description_md: Option<String>,
    #[serde(default)]
    screenshots: Vec<String>,
    #[serde(default)]
    source_url: Option<String>,
}

impl UmodPlugin {
    /// Best display name: prefer the current `title`, fall back to the legacy
    /// `name`, then the slug.
    fn display_name(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.slug.clone())
    }

    fn to_mod_info(&self) -> ModInfo {
        let category = self
            .category_tags
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        ModInfo {
            id: self.slug.clone(),
            name: self.display_name(),
            description: self.description.clone().unwrap_or_default(),
            author: self.author.clone().unwrap_or_default(),
            provider: "umod".to_string(),
            game: self.games_detail.first().map(|g| g.slug.clone()).unwrap_or_default(),
            category,
            downloads: self.downloads,
            rating: Some((self.watchers as f32 / 100.0).min(5.0)),
            latest_version: self
                .latest_release_version
                .clone()
                .unwrap_or_else(|| "0.0.0".to_string()),
            updated_at: parse_umod_date(
                self.latest_release_at_atom.as_deref(),
                self.latest_release_at.as_deref(),
            ),
            url: format!("{}/{}", UMOD_API_BASE, self.slug),
            icon_url: self.icon_url.clone(),
        }
    }
}

/// Parse a Umod timestamp, preferring the RFC3339 `*_atom` field and falling
/// back to the legacy `"YYYY-MM-DD HH:MM:SS"` (UTC) form; `now()` if neither
/// parses.
fn parse_umod_date(atom: Option<&str>, legacy: Option<&str>) -> DateTime<Utc> {
    if let Some(s) = atom {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return dt.with_timezone(&Utc);
        }
    }
    if let Some(s) = legacy {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
            return Utc.from_utc_datetime(&ndt);
        }
    }
    Utc::now()
}

/// Compute the digest of `bytes` whose hex length matches `expected`
/// (SHA-1 → 40, SHA-256 → 64). Returns `None` for an unrecognized length so
/// the caller can skip verification instead of failing on an unknown scheme.
fn digest_matching(bytes: &[u8], expected: &str) -> Option<String> {
    match expected.trim().len() {
        40 => {
            let mut hasher = Sha1::new();
            hasher.update(bytes);
            Some(format!("{:x}", hasher.finalize()))
        }
        64 => {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            Some(format!("{:x}", hasher.finalize()))
        }
        _ => None,
    }
}

/// Derive the on-disk plugin filename from its download URL. Oxide/Carbon load
/// a plugin by its class-named `.cs` file, which is the last path segment of
/// the Umod download URL (e.g. `.../AdminAntiHackFix.cs`). Falls back to
/// `<slug>.cs` when the URL is missing or unexpected.
fn plugin_file_name(download_url: Option<&str>, slug: &str) -> String {
    download_url
        .and_then(|u| u.rsplit('/').next())
        .filter(|seg| seg.ends_with(".cs") && seg.len() > 3)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}.cs", slug))
}

#[async_trait]
impl MarketplaceAdapter for UmodAdapter {
    fn provider_name(&self) -> &str {
        "umod"
    }

    fn supported_games(&self) -> Vec<String> {
        vec![
            "rust".to_string(),
            "7daystodie".to_string(),
            "ark".to_string(),
            "conanexiles".to_string(),
            "hurtworld".to_string(),
            "theforest".to_string(),
            "unturned".to_string(),
            "terraria".to_string(),
            "valheim".to_string(),
        ]
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<ModInfo>> {
        let mut url = format!(
            "{}?query={}",
            UMOD_SEARCH_API,
            urlencoding::encode(&query.query)
        );

        // Add game filter
        if let Some(ref game) = query.game {
            if let Some(slug) = Self::game_to_slug(game) {
                url.push_str(&format!("&games[]={}", slug));
            }
        }

        // Add sort order
        let sort = match query.sort {
            SortOrder::Downloads => "downloads_total",
            SortOrder::Updated => "latest_release_at",
            SortOrder::Created => "created_at",
            SortOrder::Name => "name",
            SortOrder::Rating => "watchers", // Umod uses watchers instead of ratings
        };
        url.push_str(&format!("&sort={}", sort));

        // Add pagination
        let page = (query.offset / query.limit.max(1)) + 1;
        url.push_str(&format!("&page={}&per_page={}", page, query.limit));

        debug!("Searching Umod: {}", url);

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(MarketplaceError::ApiError {
                provider: "umod".to_string(),
                message: format!("Search failed with status {}", response.status()),
                status_code: Some(response.status().as_u16()),
            });
        }

        let search_response: UmodSearchResponse = response.json().await?;

        let mods = search_response.data.iter().map(UmodPlugin::to_mod_info).collect();

        Ok(mods)
    }

    async fn get_mod_details(&self, mod_id: &str) -> Result<ModDetails> {
        let url = format!("{}/{}.json", UMOD_API_BASE, mod_id);

        debug!("Fetching Umod plugin details: {}", url);

        let response = self.client.get(&url).send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(MarketplaceError::ModNotFound(mod_id.to_string()));
        }

        if !response.status().is_success() {
            return Err(MarketplaceError::ApiError {
                provider: "umod".to_string(),
                message: format!("Failed to get mod details: {}", response.status()),
                status_code: Some(response.status().as_u16()),
            });
        }

        let plugin: UmodPlugin = response.json().await?;

        let info = plugin.to_mod_info();

        // The details endpoint no longer returns a `releases[]` history; it
        // exposes only the latest release inline (`download_url`,
        // `latest_release_version`, `latest_release_version_checksum`). Build a
        // single-version list from those fields.
        let versions: Vec<VersionInfo> = match plugin.download_url.clone() {
            Some(download_url) if !download_url.is_empty() => vec![VersionInfo {
                version: info.latest_version.clone(),
                download_url,
                file_size: 0, // Not reported by the API.
                checksum: plugin.latest_release_version_checksum.clone(),
                release_date: info.updated_at,
                changelog: None,
                min_game_version: None,
                max_game_version: None,
                downloads: plugin.downloads,
            }],
            _ => Vec::new(),
        };

        Ok(ModDetails {
            full_description: plugin.description_md.clone(),
            versions,
            dependencies: Vec::new(), // Umod doesn't expose dependency info.
            screenshots: plugin.screenshots.clone(),
            license: None,
            source_url: plugin.source_url.clone(),
            issues_url: None,
            community_url: Some("https://umod.org/community".to_string()),
            info,
        })
    }

    async fn get_categories(&self, _game: &str) -> Result<Vec<Category>> {
        // Umod categories are somewhat fixed per game
        // Return common categories
        let categories = vec![
            Category {
                id: "admin".to_string(),
                name: "Admin Tools".to_string(),
                description: Some("Server administration and moderation tools".to_string()),
                mod_count: 0,
            },
            Category {
                id: "chat".to_string(),
                name: "Chat".to_string(),
                description: Some("Chat modifications and filters".to_string()),
                mod_count: 0,
            },
            Category {
                id: "economy".to_string(),
                name: "Economy".to_string(),
                description: Some("In-game economy and shop systems".to_string()),
                mod_count: 0,
            },
            Category {
                id: "gameplay".to_string(),
                name: "Gameplay".to_string(),
                description: Some("General gameplay modifications".to_string()),
                mod_count: 0,
            },
            Category {
                id: "building".to_string(),
                name: "Building".to_string(),
                description: Some("Building and construction mods".to_string()),
                mod_count: 0,
            },
            Category {
                id: "pvp".to_string(),
                name: "PvP".to_string(),
                description: Some("Player vs player combat modifications".to_string()),
                mod_count: 0,
            },
            Category {
                id: "anti-cheat".to_string(),
                name: "Anti-Cheat".to_string(),
                description: Some("Anti-cheat and security plugins".to_string()),
                mod_count: 0,
            },
            Category {
                id: "utilities".to_string(),
                name: "Utilities".to_string(),
                description: Some("Utility and helper plugins".to_string()),
                mod_count: 0,
            },
        ];

        Ok(categories)
    }

    async fn download(
        &self,
        mod_id: &str,
        version: &str,
        target_dir: &Path,
    ) -> Result<DownloadResult> {
        // Get mod details to find download URL
        let details = self.get_mod_details(mod_id).await?;

        let version_info =
            details.versions.iter().find(|v| v.version == version).ok_or_else(|| {
                MarketplaceError::VersionNotFound {
                    mod_id: mod_id.to_string(),
                    version: version.to_string(),
                }
            })?;

        info!(
            "Downloading {} v{} from {}",
            mod_id, version, version_info.download_url
        );

        let start = Instant::now();

        // Download the file
        let response = self.client.get(&version_info.download_url).send().await?;

        if !response.status().is_success() {
            return Err(MarketplaceError::DownloadFailed {
                reason: format!("HTTP {}", response.status()),
            });
        }

        let bytes = response.bytes().await?;
        let file_size = bytes.len() as u64;

        // Our canonical record of the file is a SHA-256 digest.
        let checksum = {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            format!("{:x}", hasher.finalize())
        };

        // Verify integrity against Umod's published checksum. Umod reports a
        // SHA-1 (40 hex chars), so pick the digest by the expected length
        // rather than assuming SHA-256 (the previous code compared SHA-256 to
        // Umod's SHA-1 and therefore always failed).
        if let Some(expected) =
            version_info.checksum.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            if let Some(actual) = digest_matching(&bytes, expected) {
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(MarketplaceError::ChecksumMismatch {
                        file: mod_id.to_string(),
                        expected: expected.to_string(),
                        actual,
                    });
                }
            }
            // Unknown-length checksum: skip verification rather than falsely fail.
        }

        // Create target directory if needed
        tokio::fs::create_dir_all(target_dir).await?;

        // Write file. Oxide/Carbon load a plugin by its class-named `.cs` file,
        // which is the basename of the Umod download URL.
        let file_name = plugin_file_name(Some(&version_info.download_url), mod_id);
        let file_path = target_dir.join(&file_name);

        let mut file = tokio::fs::File::create(&file_path).await?;
        file.write_all(&bytes).await?;
        file.flush().await?;

        let download_time_ms = start.elapsed().as_millis() as u64;

        info!(
            "Downloaded {} ({} bytes) in {}ms",
            file_name, file_size, download_time_ms
        );

        Ok(DownloadResult {
            file_path,
            file_size,
            checksum,
            download_time_ms,
            // Signature keys are a DayZ/Arma (Steam Workshop) concept.
            signature_keys: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_game_to_slug() {
        assert_eq!(UmodAdapter::game_to_slug("rust"), Some("rust"));
        assert_eq!(UmodAdapter::game_to_slug("Rust"), Some("rust"));
        assert_eq!(
            UmodAdapter::game_to_slug("7daystodie"),
            Some("7-days-to-die")
        );
        assert_eq!(
            UmodAdapter::game_to_slug("ark"),
            Some("ark-survival-evolved")
        );
        assert_eq!(UmodAdapter::game_to_slug("unknown"), None);
    }

    #[test]
    fn test_supported_games() {
        let adapter = UmodAdapter::new();
        let games = adapter.supported_games();
        assert!(games.contains(&"rust".to_string()));
        assert!(games.contains(&"ark".to_string()));
    }

    // A trimmed but representative slice of a real umod.org search.json entry,
    // capturing the current field names (title/downloads/category_tags/
    // games_detail/*_atom). This is the shape that broke the old adapter.
    const SEARCH_SAMPLE: &str = r#"{
      "data": [{
        "title": "Admin Anti-Hack Fix",
        "name": "AdminAntiHackFix",
        "slug": "admin-anti-hack-fix",
        "description": "Fix for admin staff getting kicked",
        "author": "Solarix",
        "category_tags": "rust",
        "downloads": 19154,
        "watchers": 95,
        "latest_release_version": "1.0.0",
        "latest_release_at": "2022-04-03 21:43:03",
        "latest_release_at_atom": "2022-04-03T21:43:03+00:00",
        "download_url": "https://umod.org/plugins/AdminAntiHackFix.cs",
        "latest_release_version_checksum": "3cd5280ed39c78241b398fe6891a0282b845489a",
        "icon_url": "https://assets.umod.org/x.png",
        "games_detail": [{"slug": "rust", "name": "Rust"}]
      }]
    }"#;

    #[test]
    fn parses_current_search_schema() {
        let resp: UmodSearchResponse = serde_json::from_str(SEARCH_SAMPLE).unwrap();
        assert_eq!(resp.data.len(), 1);
        let info = resp.data[0].to_mod_info();
        // Display name comes from `title`, not the class `name`.
        assert_eq!(info.name, "Admin Anti-Hack Fix");
        assert_eq!(info.id, "admin-anti-hack-fix");
        assert_eq!(info.downloads, 19154);
        assert_eq!(info.category.as_deref(), Some("rust"));
        assert_eq!(info.game, "rust");
        assert_eq!(info.latest_version, "1.0.0");
        assert_eq!(info.author, "Solarix");
        // Prefers the RFC3339 `_atom` timestamp.
        assert_eq!(info.updated_at.to_rfc3339(), "2022-04-03T21:43:03+00:00");
    }

    #[test]
    fn display_name_falls_back_to_class_name_then_slug() {
        let only_name: UmodPlugin =
            serde_json::from_str(r#"{"slug":"foo","name":"FooPlugin"}"#).unwrap();
        assert_eq!(only_name.display_name(), "FooPlugin");
        let only_slug: UmodPlugin = serde_json::from_str(r#"{"slug":"foo"}"#).unwrap();
        assert_eq!(only_slug.display_name(), "foo");
    }

    #[test]
    fn legacy_downloads_total_alias_still_parses() {
        // Older search responses used `downloads_total`; the alias keeps it
        // working. (The old `category` *string* is intentionally not aliased,
        // because the details endpoint reuses `category` for an integer id.)
        let plugin: UmodPlugin =
            serde_json::from_str(r#"{"slug":"x","name":"X","downloads_total":42}"#).unwrap();
        assert_eq!(plugin.to_mod_info().downloads, 42);
    }

    #[test]
    fn details_integer_category_is_ignored_not_fatal() {
        // The details endpoint sends `category` as an integer id; it must not
        // break deserialization (previously it did, via a string alias).
        let plugin: UmodPlugin = serde_json::from_str(
            r#"{"slug":"x","name":"X","title":"X","category":2,"downloads":5,"download_url":"https://umod.org/plugins/X.cs"}"#,
        )
        .unwrap();
        let info = plugin.to_mod_info();
        assert_eq!(info.category, None);
        assert_eq!(info.downloads, 5);
    }

    #[test]
    fn details_builds_single_latest_version() {
        // The details endpoint no longer returns releases[]; the latest release
        // lives in top-level fields.
        let plugin: UmodPlugin = serde_json::from_str(SEARCH_SAMPLE)
            .map(|r: UmodSearchResponse| r.data.into_iter().next().unwrap())
            .unwrap();
        let versions: Vec<VersionInfo> = match plugin.download_url.clone() {
            Some(u) if !u.is_empty() => vec![VersionInfo {
                version: plugin.latest_release_version.clone().unwrap(),
                download_url: u,
                file_size: 0,
                checksum: plugin.latest_release_version_checksum.clone(),
                release_date: Utc::now(),
                changelog: None,
                min_game_version: None,
                max_game_version: None,
                downloads: plugin.downloads,
            }],
            _ => Vec::new(),
        };
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "1.0.0");
        assert!(versions[0].download_url.ends_with("AdminAntiHackFix.cs"));
    }

    #[test]
    fn plugin_filename_derives_from_url() {
        assert_eq!(
            plugin_file_name(
                Some("https://umod.org/plugins/AdminAntiHackFix.cs"),
                "admin-x"
            ),
            "AdminAntiHackFix.cs"
        );
        // Non-.cs or missing URL falls back to <slug>.cs.
        assert_eq!(
            plugin_file_name(Some("https://x/y/"), "admin-x"),
            "admin-x.cs"
        );
        assert_eq!(plugin_file_name(None, "admin-x"), "admin-x.cs");
    }

    #[test]
    fn digest_matching_picks_algorithm_by_length() {
        let data = b"hello world";
        // SHA-1 of "hello world"
        let sha1 = digest_matching(data, &"a".repeat(40)).unwrap();
        assert_eq!(sha1, "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed");
        // SHA-256 of "hello world"
        let sha256 = digest_matching(data, &"a".repeat(64)).unwrap();
        assert_eq!(
            sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        // Unknown length → skip (None).
        assert!(digest_matching(data, "abc").is_none());
    }

    #[test]
    fn parse_umod_date_prefers_atom_then_legacy() {
        let atom = parse_umod_date(Some("2022-04-03T21:43:03+00:00"), None);
        assert_eq!(atom.to_rfc3339(), "2022-04-03T21:43:03+00:00");
        let legacy = parse_umod_date(None, Some("2022-04-03 21:43:03"));
        assert_eq!(legacy.to_rfc3339(), "2022-04-03T21:43:03+00:00");
    }
}

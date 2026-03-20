//! Umod (formerly Oxide) marketplace adapter
//!
//! Umod is the primary mod marketplace for games like Rust, 7 Days to Die,
//! ARK, and other Unity/Unreal games.

use crate::error::{MarketplaceError, Result};
use crate::models::*;
use crate::MarketplaceAdapter;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
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

/// Umod API search response
#[derive(Debug, Deserialize)]
struct UmodSearchResponse {
    data: Vec<UmodPlugin>,
    #[allow(dead_code)]
    total_pages: Option<u32>,
}

/// Umod plugin data from API
#[derive(Debug, Deserialize)]
struct UmodPlugin {
    name: String,
    slug: String,
    description: String,
    author: String,
    #[serde(default)]
    games: Vec<String>,
    downloads_total: u64,
    latest_release_version: Option<String>,
    latest_release_at: Option<String>,
    #[serde(default)]
    category: String,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    watchers: u64,
}

/// Umod plugin details response
#[derive(Debug, Deserialize)]
struct UmodPluginDetails {
    name: String,
    slug: String,
    description: String,
    description_md: Option<String>,
    author: String,
    #[serde(default)]
    games: Vec<String>,
    downloads_total: u64,
    latest_release_version: Option<String>,
    latest_release_at: Option<String>,
    #[serde(default)]
    category: String,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    watchers: u64,
    #[serde(default)]
    releases: Vec<UmodRelease>,
    #[serde(default)]
    screenshots: Vec<String>,
    #[serde(default)]
    source_url: Option<String>,
}

/// Umod release/version data
#[derive(Debug, Deserialize)]
struct UmodRelease {
    version: String,
    download_url: String,
    #[serde(default)]
    size: u64,
    created_at: String,
    #[serde(default)]
    changelog: Option<String>,
    downloads: u64,
    checksum: Option<String>,
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

        let mods = search_response
            .data
            .into_iter()
            .map(|plugin| {
                let updated_at = plugin
                    .latest_release_at
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                ModInfo {
                    id: plugin.slug.clone(),
                    name: plugin.name,
                    description: plugin.description,
                    author: plugin.author,
                    provider: "umod".to_string(),
                    game: plugin.games.first().cloned().unwrap_or_default(),
                    category: if plugin.category.is_empty() {
                        None
                    } else {
                        Some(plugin.category)
                    },
                    downloads: plugin.downloads_total,
                    rating: Some((plugin.watchers as f32 / 100.0).min(5.0)),
                    latest_version: plugin
                        .latest_release_version
                        .unwrap_or_else(|| "0.0.0".to_string()),
                    updated_at,
                    url: format!("{}/{}", UMOD_API_BASE, plugin.slug),
                    icon_url: plugin.icon_url,
                }
            })
            .collect();

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

        let plugin: UmodPluginDetails = response.json().await?;

        let updated_at = plugin
            .latest_release_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let info = ModInfo {
            id: plugin.slug.clone(),
            name: plugin.name.clone(),
            description: plugin.description.clone(),
            author: plugin.author.clone(),
            provider: "umod".to_string(),
            game: plugin.games.first().cloned().unwrap_or_default(),
            category: if plugin.category.is_empty() {
                None
            } else {
                Some(plugin.category.clone())
            },
            downloads: plugin.downloads_total,
            rating: Some((plugin.watchers as f32 / 100.0).min(5.0)),
            latest_version: plugin.latest_release_version.unwrap_or_else(|| "0.0.0".to_string()),
            updated_at,
            url: format!("{}/{}", UMOD_API_BASE, plugin.slug),
            icon_url: plugin.icon_url,
        };

        let versions: Vec<VersionInfo> = plugin
            .releases
            .into_iter()
            .map(|release| {
                let release_date = DateTime::parse_from_rfc3339(&release.created_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                VersionInfo {
                    version: release.version,
                    download_url: release.download_url,
                    file_size: release.size,
                    checksum: release.checksum,
                    release_date,
                    changelog: release.changelog,
                    min_game_version: None,
                    max_game_version: None,
                    downloads: release.downloads,
                }
            })
            .collect();

        Ok(ModDetails {
            info,
            full_description: plugin.description_md,
            versions,
            dependencies: Vec::new(), // Umod doesn't have dependency info in API
            screenshots: plugin.screenshots,
            license: None,
            source_url: plugin.source_url,
            issues_url: None,
            community_url: Some("https://umod.org/community".to_string()),
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

        // Calculate checksum
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let checksum = format!("{:x}", hasher.finalize());

        // Verify checksum if provided
        if let Some(ref expected) = version_info.checksum {
            if checksum != *expected {
                return Err(MarketplaceError::ChecksumMismatch {
                    file: mod_id.to_string(),
                    expected: expected.clone(),
                    actual: checksum,
                });
            }
        }

        // Create target directory if needed
        tokio::fs::create_dir_all(target_dir).await?;

        // Write file
        let file_name = format!("{}.cs", mod_id); // Umod plugins are C# files
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
}

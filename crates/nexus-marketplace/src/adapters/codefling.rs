//! Codefling marketplace adapter
//!
//! Codefling is a premium marketplace for Rust game plugins,
//! offering both free and paid plugins.

// The `*Response`/`*Dto` structs below mirror the upstream Codefling JSON
// schema so serde can deserialize it. Some fields are retained for schema
// fidelity and future use even though the adapter does not surface them yet.
#![allow(dead_code)]

use crate::error::{MarketplaceError, Result};
use crate::models::*;
use crate::MarketplaceAdapter;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

const CODEFLING_API_BASE: &str = "https://codefling.com/api";
const CODEFLING_WEB_BASE: &str = "https://codefling.com";

/// Codefling marketplace adapter
pub struct CodeflingAdapter {
    client: Client,
    /// Optional API key for authenticated requests (required for purchases)
    api_key: Option<String>,
}

impl CodeflingAdapter {
    /// Create a new Codefling adapter
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent("NexusPanel/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            api_key: None,
        }
    }

    /// Create a new Codefling adapter with API key for authenticated access
    pub fn with_api_key(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .user_agent("NexusPanel/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            api_key: Some(api_key.into()),
        }
    }

    /// Build request with optional authentication
    fn build_request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut request = self.client.get(url);
        if let Some(ref key) = self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        request
    }

    /// Parse Codefling timestamp (Unix timestamp)
    fn parse_timestamp(ts: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
    }
}

impl Default for CodeflingAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Codefling API search response
#[derive(Debug, Deserialize)]
struct CodeflingSearchResponse {
    results: Vec<CodeflingProduct>,
    #[serde(default)]
    page: u32,
    #[serde(default)]
    total_results: u32,
    #[serde(default)]
    total_pages: u32,
}

/// Codefling product/plugin data
#[derive(Debug, Deserialize)]
struct CodeflingProduct {
    id: u64,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: CodeflingAuthor,
    #[serde(default)]
    category: Option<CodeflingCategory>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    reviews_count: u32,
    #[serde(default)]
    reviews_avg: f32,
    #[serde(default)]
    version: String,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    url: String,
    #[serde(default)]
    images: CodeflingImages,
    #[serde(default)]
    price: Option<CodeflingPrice>,
}

/// Codefling author info
#[derive(Debug, Deserialize, Default)]
struct CodeflingAuthor {
    #[serde(default)]
    name: String,
    #[serde(default)]
    id: u64,
}

/// Codefling category info
#[derive(Debug, Deserialize)]
struct CodeflingCategory {
    id: u64,
    name: String,
}

/// Codefling image URLs
#[derive(Debug, Deserialize, Default)]
struct CodeflingImages {
    #[serde(default)]
    thumb: Option<String>,
    #[serde(default)]
    small: Option<String>,
    #[serde(default)]
    medium: Option<String>,
    #[serde(default)]
    large: Option<String>,
}

/// Codefling price info
#[derive(Debug, Deserialize)]
struct CodeflingPrice {
    #[serde(default)]
    amount: f64,
    #[serde(default)]
    currency: String,
}

/// Codefling product details response
#[derive(Debug, Deserialize)]
struct CodeflingProductDetails {
    id: u64,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    content: String, // Full description HTML
    #[serde(default)]
    author: CodeflingAuthor,
    #[serde(default)]
    category: Option<CodeflingCategory>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    reviews_count: u32,
    #[serde(default)]
    reviews_avg: f32,
    #[serde(default)]
    version: String,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    url: String,
    #[serde(default)]
    images: CodeflingImages,
    #[serde(default)]
    screenshots: Vec<String>,
    #[serde(default)]
    files: Vec<CodeflingFile>,
    #[serde(default)]
    changelog: Option<String>,
    #[serde(default)]
    price: Option<CodeflingPrice>,
    #[serde(default)]
    support_url: Option<String>,
}

/// Codefling file/version info
#[derive(Debug, Deserialize)]
struct CodeflingFile {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    version: String,
    #[serde(default)]
    changelog: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    uploaded: i64,
    #[serde(default)]
    url: Option<String>,
}

/// Codefling categories response
#[derive(Debug, Deserialize)]
struct CodeflingCategoriesResponse {
    #[serde(default)]
    categories: Vec<CodeflingCategoryInfo>,
}

/// Codefling category with counts
#[derive(Debug, Deserialize)]
struct CodeflingCategoryInfo {
    id: u64,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    count: u64,
}

#[async_trait]
impl MarketplaceAdapter for CodeflingAdapter {
    fn provider_name(&self) -> &str {
        "codefling"
    }

    fn supported_games(&self) -> Vec<String> {
        vec!["rust".to_string()]
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<ModInfo>> {
        // Codefling is primarily Rust-focused
        if let Some(ref game) = query.game {
            if game.to_lowercase() != "rust" {
                return Err(MarketplaceError::UnsupportedGame {
                    provider: "codefling".to_string(),
                    game: game.clone(),
                });
            }
        }

        let mut url = format!(
            "{}/files?search={}&perPage={}",
            CODEFLING_API_BASE,
            urlencoding::encode(&query.query),
            query.limit
        );

        // Add pagination
        let page = (query.offset / query.limit.max(1)) + 1;
        url.push_str(&format!("&page={}", page));

        // Add sort order
        let sort = match query.sort {
            SortOrder::Downloads => "downloads",
            SortOrder::Updated => "updated",
            SortOrder::Created => "created",
            SortOrder::Name => "title",
            SortOrder::Rating => "rating",
        };
        url.push_str(&format!("&sortBy={}&sortDir=desc", sort));

        // Add category filter
        if let Some(ref category) = query.category {
            url.push_str(&format!("&category={}", urlencoding::encode(category)));
        }

        debug!("Searching Codefling: {}", url);

        let response = self.build_request(&url).send().await?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(60);
            return Err(MarketplaceError::RateLimited {
                provider: "codefling".to_string(),
                retry_after_secs: retry_after,
            });
        }

        if !response.status().is_success() {
            return Err(MarketplaceError::ApiError {
                provider: "codefling".to_string(),
                message: format!("Search failed with status {}", response.status()),
                status_code: Some(response.status().as_u16()),
            });
        }

        let search_response: CodeflingSearchResponse = response.json().await?;

        let mods = search_response
            .results
            .into_iter()
            .map(|product| {
                let updated_at = Self::parse_timestamp(product.updated);
                let price_info = product.price.as_ref().map(|p| {
                    if p.amount > 0.0 {
                        format!("${:.2} {}", p.amount, p.currency)
                    } else {
                        "Free".to_string()
                    }
                });

                ModInfo {
                    id: product.id.to_string(),
                    name: product.title,
                    description: if let Some(price) = price_info {
                        format!("[{}] {}", price, product.description)
                    } else {
                        product.description
                    },
                    author: product.author.name,
                    provider: "codefling".to_string(),
                    game: "rust".to_string(),
                    category: product.category.map(|c| c.name),
                    downloads: product.downloads,
                    rating: if product.reviews_count > 0 {
                        Some(product.reviews_avg)
                    } else {
                        None
                    },
                    latest_version: if product.version.is_empty() {
                        "1.0.0".to_string()
                    } else {
                        product.version
                    },
                    updated_at,
                    url: if product.url.is_empty() {
                        format!("{}/files/file/{}", CODEFLING_WEB_BASE, product.id)
                    } else {
                        product.url
                    },
                    icon_url: product.images.thumb.or(product.images.small),
                }
            })
            .collect();

        Ok(mods)
    }

    async fn get_mod_details(&self, mod_id: &str) -> Result<ModDetails> {
        let url = format!("{}/files/{}", CODEFLING_API_BASE, mod_id);

        debug!("Fetching Codefling product details: {}", url);

        let response = self.build_request(&url).send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(MarketplaceError::ModNotFound(mod_id.to_string()));
        }

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(60);
            return Err(MarketplaceError::RateLimited {
                provider: "codefling".to_string(),
                retry_after_secs: retry_after,
            });
        }

        if !response.status().is_success() {
            return Err(MarketplaceError::ApiError {
                provider: "codefling".to_string(),
                message: format!("Failed to get mod details: {}", response.status()),
                status_code: Some(response.status().as_u16()),
            });
        }

        let product: CodeflingProductDetails = response.json().await?;
        let updated_at = Self::parse_timestamp(product.updated);

        let info = ModInfo {
            id: product.id.to_string(),
            name: product.title.clone(),
            description: product.description.clone(),
            author: product.author.name.clone(),
            provider: "codefling".to_string(),
            game: "rust".to_string(),
            category: product.category.as_ref().map(|c| c.name.clone()),
            downloads: product.downloads,
            rating: if product.reviews_count > 0 {
                Some(product.reviews_avg)
            } else {
                None
            },
            latest_version: if product.version.is_empty() {
                "1.0.0".to_string()
            } else {
                product.version.clone()
            },
            updated_at,
            url: if product.url.is_empty() {
                format!("{}/files/file/{}", CODEFLING_WEB_BASE, product.id)
            } else {
                product.url.clone()
            },
            icon_url: product.images.thumb.clone().or(product.images.small.clone()),
        };

        // Convert files to versions
        let versions: Vec<VersionInfo> = product
            .files
            .into_iter()
            .map(|file| {
                let release_date = Self::parse_timestamp(file.uploaded);
                VersionInfo {
                    version: if file.version.is_empty() {
                        "1.0.0".to_string()
                    } else {
                        file.version
                    },
                    download_url: file.url.unwrap_or_default(),
                    file_size: file.size,
                    checksum: None, // Codefling doesn't provide checksums
                    release_date,
                    changelog: file.changelog,
                    min_game_version: None,
                    max_game_version: None,
                    downloads: file.downloads,
                }
            })
            .collect();

        Ok(ModDetails {
            info,
            full_description: Some(product.content),
            versions,
            dependencies: Vec::new(), // Would need to parse from description
            screenshots: product.screenshots,
            license: None,
            source_url: None,
            issues_url: product.support_url.clone(),
            community_url: Some(format!("{}/files/file/{}", CODEFLING_WEB_BASE, mod_id)),
        })
    }

    async fn get_categories(&self, _game: &str) -> Result<Vec<Category>> {
        let url = format!("{}/files/categories", CODEFLING_API_BASE);

        debug!("Fetching Codefling categories: {}", url);

        let response = self.build_request(&url).send().await?;

        if !response.status().is_success() {
            // Return default categories if API fails
            warn!("Failed to fetch Codefling categories, using defaults");
            return Ok(Self::default_categories());
        }

        match response.json::<CodeflingCategoriesResponse>().await {
            Ok(cat_response) => {
                let categories = cat_response
                    .categories
                    .into_iter()
                    .map(|c| Category {
                        id: c.id.to_string(),
                        name: c.name,
                        description: c.description,
                        mod_count: c.count,
                    })
                    .collect();
                Ok(categories)
            }
            Err(_) => Ok(Self::default_categories()),
        }
    }

    async fn download(
        &self,
        mod_id: &str,
        version: &str,
        target_dir: &Path,
    ) -> Result<DownloadResult> {
        // Check if we have API key for authenticated downloads
        if self.api_key.is_none() {
            warn!("Downloading from Codefling without API key - only free plugins available");
        }

        // Get mod details to find download URL
        let details = self.get_mod_details(mod_id).await?;

        let version_info = details
            .versions
            .iter()
            .find(|v| v.version == version)
            .or_else(|| details.versions.first())
            .ok_or_else(|| MarketplaceError::VersionNotFound {
                mod_id: mod_id.to_string(),
                version: version.to_string(),
            })?;

        if version_info.download_url.is_empty() {
            return Err(MarketplaceError::DownloadFailed {
                reason: "No download URL available. Plugin may require purchase.".to_string(),
            });
        }

        info!(
            "Downloading {} v{} from {}",
            mod_id, version, version_info.download_url
        );

        let start = Instant::now();

        // Download the file with authentication if available
        let response = self.build_request(&version_info.download_url).send().await?;

        if response.status() == reqwest::StatusCode::PAYMENT_REQUIRED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(MarketplaceError::DownloadFailed {
                reason: "This plugin requires purchase. Please buy it on Codefling first."
                    .to_string(),
            });
        }

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(MarketplaceError::AuthRequired {
                provider: "codefling".to_string(),
            });
        }

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

        // Create target directory if needed
        tokio::fs::create_dir_all(target_dir).await?;

        // Determine file extension based on content or default to .cs
        let file_name = format!("{}.cs", details.info.name.replace(' ', "_"));
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

impl CodeflingAdapter {
    /// Default categories for Codefling
    fn default_categories() -> Vec<Category> {
        vec![
            Category {
                id: "plugins".to_string(),
                name: "Plugins".to_string(),
                description: Some("Server plugins and mods".to_string()),
                mod_count: 0,
            },
            Category {
                id: "prefabs".to_string(),
                name: "Prefabs".to_string(),
                description: Some("Custom prefabs and monuments".to_string()),
                mod_count: 0,
            },
            Category {
                id: "maps".to_string(),
                name: "Maps".to_string(),
                description: Some("Custom server maps".to_string()),
                mod_count: 0,
            },
            Category {
                id: "skins".to_string(),
                name: "Skins".to_string(),
                description: Some("Custom item skins".to_string()),
                mod_count: 0,
            },
            Category {
                id: "tools".to_string(),
                name: "Tools".to_string(),
                description: Some("Server management tools".to_string()),
                mod_count: 0,
            },
            Category {
                id: "economy".to_string(),
                name: "Economy".to_string(),
                description: Some("Economy and shop systems".to_string()),
                mod_count: 0,
            },
            Category {
                id: "pvp".to_string(),
                name: "PvP".to_string(),
                description: Some("PvP and combat plugins".to_string()),
                mod_count: 0,
            },
            Category {
                id: "building".to_string(),
                name: "Building".to_string(),
                description: Some("Building and base protection plugins".to_string()),
                mod_count: 0,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn test_supported_games() {
        let adapter = CodeflingAdapter::new();
        let games = adapter.supported_games();
        assert!(games.contains(&"rust".to_string()));
        assert_eq!(games.len(), 1); // Codefling is Rust-only
    }

    #[test]
    fn test_provider_name() {
        let adapter = CodeflingAdapter::new();
        assert_eq!(adapter.provider_name(), "codefling");
    }

    #[test]
    fn test_parse_timestamp() {
        let ts = 1704067200; // 2024-01-01 00:00:00 UTC
        let dt = CodeflingAdapter::parse_timestamp(ts);
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn test_with_api_key() {
        let adapter = CodeflingAdapter::with_api_key("test-key");
        assert!(adapter.api_key.is_some());
        assert_eq!(adapter.api_key.unwrap(), "test-key");
    }
}

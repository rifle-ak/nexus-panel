//! Lone.Design marketplace adapter
//!
//! Lone.Design is a marketplace for Rust game plugins and resources,
//! offering both free and premium content.

// The response/DTO structs below mirror the upstream Lone.Design JSON schema
// so serde can deserialize it. Some fields are retained for schema fidelity
// and future use even though the adapter does not surface them yet.
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

const LONE_API_BASE: &str = "https://lone.design/api/v1";
const LONE_WEB_BASE: &str = "https://lone.design";

/// Lone.Design marketplace adapter
pub struct LoneDesignAdapter {
    client: Client,
    /// Optional API key for authenticated requests
    api_key: Option<String>,
}

impl LoneDesignAdapter {
    /// Create a new Lone.Design adapter
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

    /// Create a new Lone.Design adapter with API key
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
            request = request.header("X-API-Key", key);
        }
        request
    }

    /// Parse timestamp (Unix timestamp or ISO 8601)
    fn parse_timestamp(ts: &str) -> DateTime<Utc> {
        // Try parsing as Unix timestamp first
        if let Ok(unix_ts) = ts.parse::<i64>() {
            return Utc.timestamp_opt(unix_ts, 0).single().unwrap_or_else(Utc::now);
        }
        // Try parsing as ISO 8601
        DateTime::parse_from_rfc3339(ts)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now())
    }

    fn parse_unix_timestamp(ts: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
    }
}

impl Default for LoneDesignAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Lone.Design API search response
#[derive(Debug, Deserialize)]
struct LoneSearchResponse {
    #[serde(default)]
    data: Vec<LoneProduct>,
    #[serde(default)]
    meta: LonePagination,
}

/// Lone.Design pagination info
#[derive(Debug, Deserialize, Default)]
struct LonePagination {
    #[serde(default)]
    current_page: u32,
    #[serde(default)]
    total_pages: u32,
    #[serde(default)]
    total_items: u32,
    #[serde(default)]
    per_page: u32,
}

/// Lone.Design product data
#[derive(Debug, Deserialize)]
struct LoneProduct {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: LoneAuthor,
    #[serde(default)]
    category: Option<LoneCategory>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    rating: f32,
    #[serde(default)]
    rating_count: u32,
    #[serde(default)]
    version: String,
    #[serde(default)]
    updated_at: i64,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    price: f64,
    #[serde(default)]
    is_free: bool,
}

/// Lone.Design author info
#[derive(Debug, Deserialize, Default)]
struct LoneAuthor {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    username: String,
}

/// Lone.Design category info
#[derive(Debug, Deserialize)]
struct LoneCategory {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    slug: String,
}

/// Lone.Design product details
#[derive(Debug, Deserialize)]
struct LoneProductDetails {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    content: String, // Full HTML description
    #[serde(default)]
    author: LoneAuthor,
    #[serde(default)]
    category: Option<LoneCategory>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    rating: f32,
    #[serde(default)]
    rating_count: u32,
    #[serde(default)]
    version: String,
    #[serde(default)]
    updated_at: i64,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    price: f64,
    #[serde(default)]
    is_free: bool,
    #[serde(default)]
    versions: Vec<LoneVersion>,
    #[serde(default)]
    dependencies: Vec<LoneDependency>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    documentation_url: Option<String>,
    #[serde(default)]
    support_url: Option<String>,
}

/// Lone.Design version info
#[derive(Debug, Deserialize)]
struct LoneVersion {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    version: String,
    #[serde(default)]
    changelog: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    file_size: u64,
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    released_at: i64,
}

/// Lone.Design dependency info
#[derive(Debug, Deserialize)]
struct LoneDependency {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    min_version: Option<String>,
}

/// Lone.Design categories response
#[derive(Debug, Deserialize)]
struct LoneCategoriesResponse {
    #[serde(default)]
    data: Vec<LoneCategoryInfo>,
}

/// Lone.Design category with counts
#[derive(Debug, Deserialize)]
struct LoneCategoryInfo {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    product_count: u64,
}

#[async_trait]
impl MarketplaceAdapter for LoneDesignAdapter {
    fn provider_name(&self) -> &str {
        "lone_design"
    }

    fn supported_games(&self) -> Vec<String> {
        vec!["rust".to_string()]
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<ModInfo>> {
        // Lone.Design is primarily Rust-focused
        if let Some(ref game) = query.game {
            if game.to_lowercase() != "rust" {
                return Err(MarketplaceError::UnsupportedGame {
                    provider: "lone_design".to_string(),
                    game: game.clone(),
                });
            }
        }

        let mut url = format!(
            "{}/products?search={}&per_page={}",
            LONE_API_BASE,
            urlencoding::encode(&query.query),
            query.limit
        );

        // Add pagination
        let page = (query.offset / query.limit.max(1)) + 1;
        url.push_str(&format!("&page={}", page));

        // Add sort order
        let (sort_field, sort_dir) = match query.sort {
            SortOrder::Downloads => ("downloads", "desc"),
            SortOrder::Updated => ("updated_at", "desc"),
            SortOrder::Created => ("created_at", "desc"),
            SortOrder::Name => ("name", "asc"),
            SortOrder::Rating => ("rating", "desc"),
        };
        url.push_str(&format!("&sort={}&order={}", sort_field, sort_dir));

        // Add category filter
        if let Some(ref category) = query.category {
            url.push_str(&format!("&category={}", urlencoding::encode(category)));
        }

        debug!("Searching Lone.Design: {}", url);

        let response = self.build_request(&url).send().await?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(60);
            return Err(MarketplaceError::RateLimited {
                provider: "lone_design".to_string(),
                retry_after_secs: retry_after,
            });
        }

        if response.status() == reqwest::StatusCode::FORBIDDEN {
            // Lone.Design sits behind Cloudflare's bot challenge, which returns
            // a 403 "Just a moment..." interstitial to non-browser clients.
            return Err(MarketplaceError::ApiError {
                provider: "lone_design".to_string(),
                message: "Blocked by Cloudflare bot protection (403). Lone.Design is not \
                          reachable from a plain HTTP client without a browser/JS challenge."
                    .to_string(),
                status_code: Some(403),
            });
        }

        if !response.status().is_success() {
            return Err(MarketplaceError::ApiError {
                provider: "lone_design".to_string(),
                message: format!("Search failed with status {}", response.status()),
                status_code: Some(response.status().as_u16()),
            });
        }

        let search_response: LoneSearchResponse = response.json().await?;

        let mods = search_response
            .data
            .into_iter()
            .map(|product| {
                let updated_at = Self::parse_unix_timestamp(product.updated_at);
                let price_info = if product.is_free || product.price == 0.0 {
                    "Free".to_string()
                } else {
                    format!("${:.2}", product.price)
                };

                ModInfo {
                    id: if product.slug.is_empty() {
                        product.id.to_string()
                    } else {
                        product.slug
                    },
                    name: product.name,
                    description: format!("[{}] {}", price_info, product.description),
                    author: if product.author.name.is_empty() {
                        product.author.username
                    } else {
                        product.author.name
                    },
                    provider: "lone_design".to_string(),
                    game: "rust".to_string(),
                    category: product.category.map(|c| c.name),
                    downloads: product.downloads,
                    rating: if product.rating_count > 0 {
                        Some(product.rating)
                    } else {
                        None
                    },
                    latest_version: if product.version.is_empty() {
                        "1.0.0".to_string()
                    } else {
                        product.version
                    },
                    updated_at,
                    url: format!("{}/product/{}", LONE_WEB_BASE, product.id),
                    icon_url: product.thumbnail,
                }
            })
            .collect();

        Ok(mods)
    }

    async fn get_mod_details(&self, mod_id: &str) -> Result<ModDetails> {
        let url = format!("{}/products/{}", LONE_API_BASE, mod_id);

        debug!("Fetching Lone.Design product details: {}", url);

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
                provider: "lone_design".to_string(),
                retry_after_secs: retry_after,
            });
        }

        if !response.status().is_success() {
            return Err(MarketplaceError::ApiError {
                provider: "lone_design".to_string(),
                message: format!("Failed to get mod details: {}", response.status()),
                status_code: Some(response.status().as_u16()),
            });
        }

        let product: LoneProductDetails = response.json().await?;
        let updated_at = Self::parse_unix_timestamp(product.updated_at);

        let info = ModInfo {
            id: if product.slug.is_empty() {
                product.id.to_string()
            } else {
                product.slug.clone()
            },
            name: product.name.clone(),
            description: product.description.clone(),
            author: if product.author.name.is_empty() {
                product.author.username.clone()
            } else {
                product.author.name.clone()
            },
            provider: "lone_design".to_string(),
            game: "rust".to_string(),
            category: product.category.as_ref().map(|c| c.name.clone()),
            downloads: product.downloads,
            rating: if product.rating_count > 0 {
                Some(product.rating)
            } else {
                None
            },
            latest_version: if product.version.is_empty() {
                "1.0.0".to_string()
            } else {
                product.version.clone()
            },
            updated_at,
            url: format!("{}/product/{}", LONE_WEB_BASE, product.id),
            icon_url: product.thumbnail.clone(),
        };

        // Convert versions
        let versions: Vec<VersionInfo> = product
            .versions
            .into_iter()
            .map(|v| {
                let release_date = Self::parse_unix_timestamp(v.released_at);
                VersionInfo {
                    version: if v.version.is_empty() {
                        "1.0.0".to_string()
                    } else {
                        v.version
                    },
                    download_url: v.download_url.unwrap_or_default(),
                    file_size: v.file_size,
                    checksum: None,
                    release_date,
                    changelog: v.changelog,
                    min_game_version: None,
                    max_game_version: None,
                    downloads: v.downloads,
                }
            })
            .collect();

        // Convert dependencies
        let dependencies: Vec<Dependency> = product
            .dependencies
            .into_iter()
            .map(|d| Dependency {
                mod_id: if d.slug.is_empty() {
                    d.id.to_string()
                } else {
                    d.slug
                },
                name: d.name,
                required: d.required,
                min_version: d.min_version,
            })
            .collect();

        Ok(ModDetails {
            info,
            full_description: Some(product.content),
            versions,
            dependencies,
            screenshots: product.images,
            license: None,
            source_url: None,
            issues_url: product.support_url,
            community_url: Some(format!("{}/product/{}", LONE_WEB_BASE, mod_id)),
        })
    }

    async fn get_categories(&self, _game: &str) -> Result<Vec<Category>> {
        let url = format!("{}/categories", LONE_API_BASE);

        debug!("Fetching Lone.Design categories: {}", url);

        let response = self.build_request(&url).send().await?;

        if !response.status().is_success() {
            warn!("Failed to fetch Lone.Design categories, using defaults");
            return Ok(Self::default_categories());
        }

        match response.json::<LoneCategoriesResponse>().await {
            Ok(cat_response) => {
                let categories = cat_response
                    .data
                    .into_iter()
                    .map(|c| Category {
                        id: if c.slug.is_empty() {
                            c.id.to_string()
                        } else {
                            c.slug
                        },
                        name: c.name,
                        description: c.description,
                        mod_count: c.product_count,
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
        if self.api_key.is_none() {
            warn!("Downloading from Lone.Design without API key - only free products available");
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
                reason: "No download URL available. Product may require purchase.".to_string(),
            });
        }

        info!(
            "Downloading {} v{} from {}",
            mod_id, version, version_info.download_url
        );

        let start = Instant::now();

        // Download the file
        let response = self.build_request(&version_info.download_url).send().await?;

        if response.status() == reqwest::StatusCode::PAYMENT_REQUIRED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(MarketplaceError::DownloadFailed {
                reason: "This product requires purchase. Please buy it on Lone.Design first."
                    .to_string(),
            });
        }

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(MarketplaceError::AuthRequired {
                provider: "lone_design".to_string(),
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

        // Create target directory
        tokio::fs::create_dir_all(target_dir).await?;

        // Write file
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
            // Signature keys are a DayZ/Arma (Steam Workshop) concept.
            signature_keys: Vec::new(),
        })
    }
}

impl LoneDesignAdapter {
    /// Default categories
    fn default_categories() -> Vec<Category> {
        vec![
            Category {
                id: "plugins".to_string(),
                name: "Plugins".to_string(),
                description: Some("Server plugins and modifications".to_string()),
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
                id: "tools".to_string(),
                name: "Tools".to_string(),
                description: Some("Development and admin tools".to_string()),
                mod_count: 0,
            },
            Category {
                id: "scripts".to_string(),
                name: "Scripts".to_string(),
                description: Some("Server scripts and utilities".to_string()),
                mod_count: 0,
            },
            Category {
                id: "resources".to_string(),
                name: "Resources".to_string(),
                description: Some("Graphics, sounds, and other assets".to_string()),
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
        let adapter = LoneDesignAdapter::new();
        let games = adapter.supported_games();
        assert!(games.contains(&"rust".to_string()));
        assert_eq!(games.len(), 1);
    }

    #[test]
    fn test_provider_name() {
        let adapter = LoneDesignAdapter::new();
        assert_eq!(adapter.provider_name(), "lone_design");
    }

    #[test]
    fn test_parse_timestamp() {
        // Test Unix timestamp
        let dt = LoneDesignAdapter::parse_timestamp("1704067200");
        assert_eq!(dt.year(), 2024);

        // Test ISO 8601
        let dt = LoneDesignAdapter::parse_timestamp("2024-01-01T00:00:00Z");
        assert_eq!(dt.year(), 2024);
    }

    #[test]
    fn test_parse_unix_timestamp() {
        let ts = 1704067200;
        let dt = LoneDesignAdapter::parse_unix_timestamp(ts);
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn test_with_api_key() {
        let adapter = LoneDesignAdapter::with_api_key("test-key");
        assert!(adapter.api_key.is_some());
        assert_eq!(adapter.api_key.unwrap(), "test-key");
    }
}

//! Data models for marketplace operations

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Search query for finding mods
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Search text (mod name, description, author)
    pub query: String,

    /// Filter by game type (e.g., "rust", "minecraft", "csgo")
    pub game: Option<String>,

    /// Filter by category
    pub category: Option<String>,

    /// Sort order
    pub sort: SortOrder,

    /// Maximum number of results
    pub limit: usize,

    /// Offset for pagination
    pub offset: usize,
}

impl SearchQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: 25,
            ..Default::default()
        }
    }

    pub fn with_game(mut self, game: impl Into<String>) -> Self {
        self.game = Some(game.into());
        self
    }

    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn with_sort(mut self, sort: SortOrder) -> Self {
        self.sort = sort;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }
}

/// Sort order for search results
#[derive(Debug, Clone, Default)]
pub enum SortOrder {
    /// Most downloaded first
    #[default]
    Downloads,
    /// Most recently updated first
    Updated,
    /// Newest first
    Created,
    /// Alphabetical by name
    Name,
    /// Highest rated first
    Rating,
}

/// Basic mod information (returned in search results)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModInfo {
    /// Unique identifier within the marketplace
    pub id: String,

    /// Display name
    pub name: String,

    /// Short description
    pub description: String,

    /// Author name
    pub author: String,

    /// Marketplace provider (e.g., "umod", "curseforge")
    pub provider: String,

    /// Game this mod is for
    pub game: String,

    /// Category/tag
    pub category: Option<String>,

    /// Total downloads
    pub downloads: u64,

    /// Rating (0.0 - 5.0)
    pub rating: Option<f32>,

    /// Latest version string
    pub latest_version: String,

    /// Last updated timestamp
    pub updated_at: DateTime<Utc>,

    /// URL to mod page
    pub url: String,

    /// Icon/thumbnail URL
    pub icon_url: Option<String>,
}

/// Detailed mod information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModDetails {
    /// Basic mod info
    #[serde(flatten)]
    pub info: ModInfo,

    /// Full description (may include HTML/Markdown)
    pub full_description: Option<String>,

    /// Available versions
    pub versions: Vec<VersionInfo>,

    /// Dependencies
    pub dependencies: Vec<Dependency>,

    /// Screenshots
    pub screenshots: Vec<String>,

    /// License information
    pub license: Option<String>,

    /// Source code URL (if open source)
    pub source_url: Option<String>,

    /// Issues/bug tracker URL
    pub issues_url: Option<String>,

    /// Discord/community URL
    pub community_url: Option<String>,
}

/// Version information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Version string (e.g., "1.2.3")
    pub version: String,

    /// Download URL
    pub download_url: String,

    /// File size in bytes
    pub file_size: u64,

    /// File checksum (SHA256)
    pub checksum: Option<String>,

    /// Release date
    pub release_date: DateTime<Utc>,

    /// Changelog for this version
    pub changelog: Option<String>,

    /// Minimum game version required
    pub min_game_version: Option<String>,

    /// Maximum game version supported
    pub max_game_version: Option<String>,

    /// Download count for this version
    pub downloads: u64,
}

/// Mod dependency
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    /// Dependency mod ID
    pub mod_id: String,

    /// Dependency name
    pub name: String,

    /// Whether this dependency is required
    pub required: bool,

    /// Minimum version required
    pub min_version: Option<String>,
}

/// Installed mod information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledMod {
    /// Marketplace provider
    pub provider: String,

    /// Mod ID
    pub mod_id: String,

    /// Mod name
    pub name: String,

    /// Installed version
    pub version: String,

    /// Installation path
    pub path: PathBuf,

    /// Installation timestamp
    pub installed_at: DateTime<Utc>,

    /// Last updated timestamp
    pub updated_at: DateTime<Utc>,

    /// Whether auto-update is enabled
    pub auto_update: bool,
}

/// Update information for an installed mod
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModUpdate {
    /// Currently installed mod
    pub installed: InstalledMod,

    /// Latest available version
    pub latest_version: String,

    /// Changelog for the update
    pub changelog: Option<String>,

    /// Release date of the new version
    pub release_date: DateTime<Utc>,
}

/// Result of a mod download operation
#[derive(Debug, Clone, Default)]
pub struct DownloadResult {
    /// Downloaded file path
    pub file_path: PathBuf,

    /// File size in bytes
    pub file_size: u64,

    /// File checksum (SHA256)
    pub checksum: String,

    /// Time taken to download
    pub download_time_ms: u64,

    /// Signature keys the install placed in the server's `keys/` directory.
    ///
    /// Only the Real Virtuality / Enfusion games (DayZ, Arma) use these: a mod
    /// ships `.bikey` files that must sit in the *server's* key directory, or
    /// clients are rejected when the server verifies signatures. Empty for
    /// every other provider.
    pub signature_keys: Vec<String>,
}

/// Mod category
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    /// Category ID
    pub id: String,

    /// Display name
    pub name: String,

    /// Description
    pub description: Option<String>,

    /// Number of mods in this category
    pub mod_count: u64,
}

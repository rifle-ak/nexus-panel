//! Marketplace adapters for different providers

pub mod codefling;
pub mod lone_design;
pub mod umod;

use crate::error::Result;
use crate::models::*;
use async_trait::async_trait;
use std::path::Path;

pub use codefling::CodeflingAdapter;
pub use lone_design::LoneDesignAdapter;
pub use umod::UmodAdapter;

/// Trait for marketplace provider adapters
#[async_trait]
pub trait MarketplaceAdapter: Send + Sync {
    /// Get the provider name (e.g., "umod", "curseforge")
    fn provider_name(&self) -> &str;

    /// Get the list of supported games
    fn supported_games(&self) -> Vec<String>;

    /// Search for mods
    async fn search(&self, query: &SearchQuery) -> Result<Vec<ModInfo>>;

    /// Get detailed mod information
    async fn get_mod_details(&self, mod_id: &str) -> Result<ModDetails>;

    /// Get available categories for a game
    async fn get_categories(&self, game: &str) -> Result<Vec<Category>>;

    /// Download a specific version of a mod
    async fn download(
        &self,
        mod_id: &str,
        version: &str,
        target_dir: &Path,
    ) -> Result<DownloadResult>;
}

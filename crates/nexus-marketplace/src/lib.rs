//! Nexus Marketplace - Mod marketplace integration for game servers
//!
//! This crate provides a unified interface for downloading and managing game server
//! mods from various marketplace providers like Umod, CurseForge, Steam Workshop, etc.

pub mod adapters;
pub mod error;
pub mod models;
pub mod cache;

pub use adapters::MarketplaceAdapter;
pub use error::{MarketplaceError, Result};
pub use models::*;

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Marketplace manager that handles multiple marketplace adapters
pub struct MarketplaceManager {
    /// Registered marketplace adapters by provider name
    adapters: HashMap<String, Arc<dyn MarketplaceAdapter>>,

    /// Cache for mod metadata
    cache: Arc<RwLock<cache::MetadataCache>>,
}

impl MarketplaceManager {
    /// Create a new marketplace manager
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
            cache: Arc::new(RwLock::new(cache::MetadataCache::new())),
        }
    }

    /// Register a marketplace adapter
    pub fn register_adapter<A: MarketplaceAdapter + 'static>(&mut self, adapter: A) {
        let provider = adapter.provider_name().to_string();
        info!("Registering marketplace adapter: {}", provider);
        self.adapters.insert(provider, Arc::new(adapter));
    }

    /// Get a list of registered providers
    pub fn providers(&self) -> Vec<String> {
        self.adapters.keys().cloned().collect()
    }

    /// Search for mods across all registered marketplaces
    pub async fn search_all(&self, query: &SearchQuery) -> Result<Vec<ModInfo>> {
        let mut results = Vec::new();

        for (provider, adapter) in &self.adapters {
            match adapter.search(query).await {
                Ok(mods) => {
                    info!("Found {} mods from {}", mods.len(), provider);
                    results.extend(mods);
                }
                Err(e) => {
                    warn!("Failed to search {}: {}", provider, e);
                }
            }
        }

        // Sort by download count (popularity)
        results.sort_by(|a, b| b.downloads.cmp(&a.downloads));

        Ok(results)
    }

    /// Search for mods from a specific provider
    pub async fn search(&self, provider: &str, query: &SearchQuery) -> Result<Vec<ModInfo>> {
        let adapter = self.adapters.get(provider).ok_or_else(|| {
            MarketplaceError::UnknownProvider(provider.to_string())
        })?;

        adapter.search(query).await
    }

    /// Get mod details by ID
    pub async fn get_mod(&self, provider: &str, mod_id: &str) -> Result<ModDetails> {
        // Check cache first
        let cache_key = format!("{}:{}", provider, mod_id);
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get(&cache_key) {
                return Ok(cached.clone());
            }
        }

        let adapter = self.adapters.get(provider).ok_or_else(|| {
            MarketplaceError::UnknownProvider(provider.to_string())
        })?;

        let details = adapter.get_mod_details(mod_id).await?;

        // Cache the result
        {
            let mut cache = self.cache.write().await;
            cache.set(cache_key, details.clone());
        }

        Ok(details)
    }

    /// Download a mod to the specified directory
    pub async fn download_mod(
        &self,
        provider: &str,
        mod_id: &str,
        version: Option<&str>,
        target_dir: &Path,
    ) -> Result<DownloadResult> {
        let adapter = self.adapters.get(provider).ok_or_else(|| {
            MarketplaceError::UnknownProvider(provider.to_string())
        })?;

        let details = self.get_mod(provider, mod_id).await?;

        // Find the version to download
        let version_info = if let Some(v) = version {
            details.versions.iter().find(|ver| ver.version == v)
        } else {
            details.versions.first() // Latest version
        };

        let version_info = version_info.ok_or_else(|| {
            MarketplaceError::VersionNotFound {
                mod_id: mod_id.to_string(),
                version: version.unwrap_or("latest").to_string(),
            }
        })?;

        info!(
            "Downloading mod {} version {} from {}",
            details.info.name,
            version_info.version,
            provider
        );

        adapter.download(mod_id, &version_info.version, target_dir).await
    }

    /// Check for updates for installed mods
    pub async fn check_updates(&self, installed: &[InstalledMod]) -> Result<Vec<ModUpdate>> {
        let mut updates = Vec::new();

        for installed_mod in installed {
            if let Some(adapter) = self.adapters.get(&installed_mod.provider) {
                match adapter.get_mod_details(&installed_mod.mod_id).await {
                    Ok(details) => {
                        if let Some(latest) = details.versions.first() {
                            if latest.version != installed_mod.version {
                                updates.push(ModUpdate {
                                    installed: installed_mod.clone(),
                                    latest_version: latest.version.clone(),
                                    changelog: latest.changelog.clone(),
                                    release_date: latest.release_date,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to check updates for {}/{}: {}",
                            installed_mod.provider, installed_mod.mod_id, e
                        );
                    }
                }
            }
        }

        Ok(updates)
    }
}

impl Default for MarketplaceManager {
    fn default() -> Self {
        Self::new()
    }
}

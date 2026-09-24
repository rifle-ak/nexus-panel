//! Nexus Marketplace - Mod marketplace integration for game servers
//!
//! This crate provides a unified interface for downloading and managing game server
//! mods from various marketplace providers like Umod, CurseForge, Steam Workshop, etc.

pub mod adapters;
pub mod cache;
pub mod error;
pub mod models;

pub use adapters::MarketplaceAdapter;
pub use error::{MarketplaceError, Result};
pub use models::*;

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

        // Sort by download count (popularity), descending.
        results.sort_by_key(|m| std::cmp::Reverse(m.downloads));

        Ok(results)
    }

    /// Search for mods from a specific provider
    pub async fn search(&self, provider: &str, query: &SearchQuery) -> Result<Vec<ModInfo>> {
        let adapter = self
            .adapters
            .get(provider)
            .ok_or_else(|| MarketplaceError::UnknownProvider(provider.to_string()))?;

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

        let adapter = self
            .adapters
            .get(provider)
            .ok_or_else(|| MarketplaceError::UnknownProvider(provider.to_string()))?;

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
        let adapter = self
            .adapters
            .get(provider)
            .ok_or_else(|| MarketplaceError::UnknownProvider(provider.to_string()))?;

        let details = self.get_mod(provider, mod_id).await?;

        // Find the version to download
        let version_info = if let Some(v) = version {
            details.versions.iter().find(|ver| ver.version == v)
        } else {
            details.versions.first() // Latest version
        };

        let version_info = version_info.ok_or_else(|| MarketplaceError::VersionNotFound {
            mod_id: mod_id.to_string(),
            version: version.unwrap_or("latest").to_string(),
        })?;

        info!(
            "Downloading mod {} version {} from {}",
            details.info.name, version_info.version, provider
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
                        // A check fetches fresh details; hand them to the cache so
                        // the update that follows downloads what the check saw
                        // rather than a version cached before the release.
                        {
                            let mut cache = self.cache.write().await;
                            cache.set(
                                format!("{}:{}", installed_mod.provider, installed_mod.mod_id),
                                details.clone(),
                            );
                        }
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

    /// Resolve dependencies for a mod
    ///
    /// Returns a list of dependencies that need to be installed, including transitive dependencies.
    /// Dependencies are returned in installation order (dependencies first).
    pub async fn resolve_dependencies(
        &self,
        provider: &str,
        mod_id: &str,
        installed: &[InstalledMod],
    ) -> Result<Vec<DependencyResolution>> {
        let mut resolutions = Vec::new();
        let mut visited = std::collections::HashSet::new();

        self.resolve_dependencies_recursive(
            provider,
            mod_id,
            installed,
            &mut resolutions,
            &mut visited,
        )
        .await?;

        Ok(resolutions)
    }

    /// Recursive helper for dependency resolution
    async fn resolve_dependencies_recursive(
        &self,
        provider: &str,
        mod_id: &str,
        installed: &[InstalledMod],
        resolutions: &mut Vec<DependencyResolution>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        let key = format!("{}:{}", provider, mod_id);
        if visited.contains(&key) {
            return Ok(()); // Already processed
        }
        visited.insert(key);

        let details = self.get_mod(provider, mod_id).await?;

        for dep in &details.dependencies {
            // Check if dependency is already installed
            let is_installed =
                installed.iter().any(|m| m.provider == provider && m.mod_id == dep.mod_id);

            let needs_update = if is_installed {
                // Check if installed version meets minimum requirement
                if let Some(ref min_ver) = dep.min_version {
                    installed
                        .iter()
                        .find(|m| m.provider == provider && m.mod_id == dep.mod_id)
                        .map(|m| version_compare(&m.version, min_ver) < 0)
                        .unwrap_or(false)
                } else {
                    false
                }
            } else {
                false
            };

            // Recursively resolve sub-dependencies first
            Box::pin(self.resolve_dependencies_recursive(
                provider,
                &dep.mod_id,
                installed,
                resolutions,
                visited,
            ))
            .await?;

            // Add this dependency if not installed or needs update
            if !is_installed || needs_update {
                resolutions.push(DependencyResolution {
                    mod_id: dep.mod_id.clone(),
                    name: dep.name.clone(),
                    provider: provider.to_string(),
                    required: dep.required,
                    min_version: dep.min_version.clone(),
                    action: if needs_update {
                        DependencyAction::Update
                    } else {
                        DependencyAction::Install
                    },
                });
            }
        }

        Ok(())
    }

    /// Auto-update all installed mods that have updates available
    ///
    /// Returns a list of successfully updated mods and any errors encountered.
    pub async fn auto_update(
        &self,
        installed: &[InstalledMod],
        target_dir: &Path,
    ) -> Result<AutoUpdateResult> {
        let updates = self.check_updates(installed).await?;
        let mut result = AutoUpdateResult {
            updated: Vec::new(),
            failed: Vec::new(),
            skipped: Vec::new(),
        };

        for update in updates {
            // Only update if auto-update is enabled for this mod
            if !update.installed.auto_update {
                result.skipped.push(AutoUpdateSkipped {
                    mod_id: update.installed.mod_id.clone(),
                    provider: update.installed.provider.clone(),
                    reason: "Auto-update disabled".to_string(),
                });
                continue;
            }

            info!(
                "Auto-updating {} from {} to {}",
                update.installed.name, update.installed.version, update.latest_version
            );

            match self
                .download_mod(
                    &update.installed.provider,
                    &update.installed.mod_id,
                    Some(&update.latest_version),
                    target_dir,
                )
                .await
            {
                Ok(download_result) => {
                    result.updated.push(AutoUpdateSuccess {
                        mod_id: update.installed.mod_id.clone(),
                        provider: update.installed.provider.clone(),
                        name: update.installed.name.clone(),
                        old_version: update.installed.version.clone(),
                        new_version: update.latest_version.clone(),
                        file_path: download_result.file_path,
                    });
                }
                Err(e) => {
                    result.failed.push(AutoUpdateFailure {
                        mod_id: update.installed.mod_id.clone(),
                        provider: update.installed.provider.clone(),
                        error: e.to_string(),
                    });
                }
            }
        }

        Ok(result)
    }

    /// Get categories for a game from a specific provider
    pub async fn get_categories(&self, provider: &str, game: &str) -> Result<Vec<Category>> {
        let adapter = self
            .adapters
            .get(provider)
            .ok_or_else(|| MarketplaceError::UnknownProvider(provider.to_string()))?;

        adapter.get_categories(game).await
    }

    /// Get supported games across all providers
    pub fn supported_games(&self) -> Vec<String> {
        let mut games = std::collections::HashSet::new();
        for adapter in self.adapters.values() {
            for game in adapter.supported_games() {
                games.insert(game);
            }
        }
        games.into_iter().collect()
    }
}

impl Default for MarketplaceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Dependency resolution result
#[derive(Debug, Clone)]
pub struct DependencyResolution {
    /// Mod ID of the dependency
    pub mod_id: String,
    /// Name of the dependency
    pub name: String,
    /// Provider for this dependency
    pub provider: String,
    /// Whether this dependency is required
    pub required: bool,
    /// Minimum version required
    pub min_version: Option<String>,
    /// Action to take
    pub action: DependencyAction,
}

/// Action to take for a dependency
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyAction {
    /// Install the dependency (not currently installed)
    Install,
    /// Update the dependency (installed but version too old)
    Update,
}

/// Result of auto-update operation
#[derive(Debug, Clone)]
pub struct AutoUpdateResult {
    /// Successfully updated mods
    pub updated: Vec<AutoUpdateSuccess>,
    /// Failed updates
    pub failed: Vec<AutoUpdateFailure>,
    /// Skipped mods (auto-update disabled or other reason)
    pub skipped: Vec<AutoUpdateSkipped>,
}

/// Successfully updated mod info
#[derive(Debug, Clone)]
pub struct AutoUpdateSuccess {
    /// Mod ID
    pub mod_id: String,
    /// Provider
    pub provider: String,
    /// Mod name
    pub name: String,
    /// Previous version
    pub old_version: String,
    /// New version
    pub new_version: String,
    /// Path to downloaded file
    pub file_path: std::path::PathBuf,
}

/// Failed update info
#[derive(Debug, Clone)]
pub struct AutoUpdateFailure {
    /// Mod ID
    pub mod_id: String,
    /// Provider
    pub provider: String,
    /// Error message
    pub error: String,
}

/// Skipped update info
#[derive(Debug, Clone)]
pub struct AutoUpdateSkipped {
    /// Mod ID
    pub mod_id: String,
    /// Provider
    pub provider: String,
    /// Reason for skipping
    pub reason: String,
}

/// Simple version comparison (semver-like)
/// Returns: -1 if a < b, 0 if a == b, 1 if a > b
fn version_compare(a: &str, b: &str) -> i32 {
    let parse_parts = |v: &str| -> Vec<u32> {
        v.split('.')
            .filter_map(|p| {
                p.trim_start_matches(|c: char| !c.is_ascii_digit())
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|s| s.parse().ok())
            })
            .collect()
    };

    let parts_a = parse_parts(a);
    let parts_b = parse_parts(b);

    for i in 0..std::cmp::max(parts_a.len(), parts_b.len()) {
        let pa = parts_a.get(i).copied().unwrap_or(0);
        let pb = parts_b.get(i).copied().unwrap_or(0);

        if pa < pb {
            return -1;
        } else if pa > pb {
            return 1;
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_compare() {
        assert_eq!(version_compare("1.0.0", "1.0.0"), 0);
        assert_eq!(version_compare("1.0.0", "2.0.0"), -1);
        assert_eq!(version_compare("2.0.0", "1.0.0"), 1);
        assert_eq!(version_compare("1.0", "1.0.0"), 0);
        assert_eq!(version_compare("1.2.3", "1.2.4"), -1);
        assert_eq!(version_compare("1.10.0", "1.9.0"), 1);
        assert_eq!(version_compare("v1.0.0", "1.0.0"), 0);
    }
}

//! Configuration hot reload support for enterprise operations.
//!
//! Provides:
//! - File-based configuration watching
//! - Atomic configuration updates
//! - Validation before reload
//! - Change notifications
//! - Rollback on failure
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::config_reload::{ConfigWatcher, ReloadableConfig};
//!
//! let watcher = ConfigWatcher::new("/etc/nexus/config.yaml").await?;
//! watcher.on_change(|config| {
//!     println!("Configuration updated!");
//! });
//! ```

use arc_swap::ArcSwap;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Configuration reload errors
#[derive(Error, Debug)]
pub enum ReloadError {
    #[error("Failed to read configuration file: {path}")]
    ReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse configuration: {0}")]
    ParseError(String),

    #[error("Configuration validation failed: {0}")]
    ValidationError(String),

    #[error("Failed to watch file: {0}")]
    WatchError(String),

    #[error("Reload in progress")]
    ReloadInProgress,
}

/// Reloadable configuration trait
pub trait ReloadableConfig: Clone + Send + Sync + 'static {
    /// Validate the configuration
    fn validate(&self) -> Result<(), String>;

    /// Merge with another configuration (for partial updates)
    fn merge(&mut self, other: &Self);

    /// Called after successful reload
    fn on_reload(&self) {}
}

/// Configuration holder with atomic swap
pub struct ConfigHolder<T: ReloadableConfig> {
    current: ArcSwap<T>,
    previous: RwLock<Option<Arc<T>>>,
    reload_count: std::sync::atomic::AtomicU64,
    last_reload: RwLock<Option<Instant>>,
}

impl<T: ReloadableConfig> ConfigHolder<T> {
    /// Create a new config holder
    pub fn new(config: T) -> Self {
        Self {
            current: ArcSwap::from_pointee(config),
            previous: RwLock::new(None),
            reload_count: std::sync::atomic::AtomicU64::new(0),
            last_reload: RwLock::new(None),
        }
    }

    /// Get the current configuration
    pub fn get(&self) -> Arc<T> {
        self.current.load_full()
    }

    /// Update the configuration
    pub fn update(&self, new_config: T) -> Result<(), ReloadError> {
        // Validate new configuration
        new_config
            .validate()
            .map_err(ReloadError::ValidationError)?;

        // Store previous for rollback
        let previous = self.current.load_full();
        *self.previous.write() = Some(previous);

        // Swap atomically
        self.current.store(Arc::new(new_config));
        self.reload_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_reload.write() = Some(Instant::now());

        // Notify
        self.get().on_reload();

        info!("Configuration updated successfully");
        Ok(())
    }

    /// Rollback to previous configuration
    pub fn rollback(&self) -> bool {
        if let Some(previous) = self.previous.write().take() {
            self.current.store(previous);
            warn!("Configuration rolled back");
            true
        } else {
            false
        }
    }

    /// Get reload statistics
    pub fn stats(&self) -> ConfigStats {
        ConfigStats {
            reload_count: self
                .reload_count
                .load(std::sync::atomic::Ordering::SeqCst),
            last_reload: *self.last_reload.read(),
            has_previous: self.previous.read().is_some(),
        }
    }
}

/// Configuration statistics
#[derive(Debug, Clone)]
pub struct ConfigStats {
    pub reload_count: u64,
    pub last_reload: Option<Instant>,
    pub has_previous: bool,
}

/// Configuration file watcher
pub struct ConfigWatcher<T: ReloadableConfig + DeserializeOwned> {
    config: Arc<ConfigHolder<T>>,
    path: PathBuf,
    watcher: Option<RecommendedWatcher>,
    change_tx: broadcast::Sender<()>,
    debounce: Duration,
    last_event: RwLock<Option<Instant>>,
}

impl<T: ReloadableConfig + DeserializeOwned> ConfigWatcher<T> {
    /// Create a new configuration watcher
    pub fn new(path: impl AsRef<Path>, initial: T) -> Result<Self, ReloadError> {
        let path = path.as_ref().to_path_buf();
        let (change_tx, _) = broadcast::channel(16);

        Ok(Self {
            config: Arc::new(ConfigHolder::new(initial)),
            path,
            watcher: None,
            change_tx,
            debounce: Duration::from_millis(500),
            last_event: RwLock::new(None),
        })
    }

    /// Load configuration from file
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, ReloadError> {
        let path = path.as_ref();
        let content = fs::read_to_string(path).map_err(|e| ReloadError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let config: T =
            serde_yaml::from_str(&content).map_err(|e| ReloadError::ParseError(e.to_string()))?;

        config.validate().map_err(ReloadError::ValidationError)?;

        Self::new(path, config)
    }

    /// Start watching for changes
    pub fn start_watching(&mut self) -> Result<(), ReloadError> {
        let path = self.path.clone();
        let config = self.config.clone();
        let change_tx = self.change_tx.clone();
        let debounce = self.debounce;
        let last_event = Arc::new(RwLock::new(None::<Instant>));
        let last_event_clone = last_event.clone();

        let mut watcher =
            notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
                match result {
                    Ok(event) => {
                        if event.kind.is_modify() || event.kind.is_create() {
                            // Debounce
                            let now = Instant::now();
                            let should_process = {
                                let last = last_event_clone.read();
                                last.map_or(true, |t| now.duration_since(t) >= debounce)
                            };

                            if should_process {
                                *last_event_clone.write() = Some(now);
                                debug!("Configuration file changed, reloading...");

                                if let Err(e) = Self::reload_internal(&path, &config) {
                                    error!("Failed to reload configuration: {}", e);
                                } else {
                                    let _ = change_tx.send(());
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Watch error: {}", e);
                    }
                }
            })
            .map_err(|e| ReloadError::WatchError(e.to_string()))?;

        // Watch the file's parent directory (more reliable)
        let watch_path = self.path.parent().unwrap_or(&self.path);
        watcher
            .watch(watch_path, RecursiveMode::NonRecursive)
            .map_err(|e| ReloadError::WatchError(e.to_string()))?;

        self.watcher = Some(watcher);
        info!("Started watching configuration file: {:?}", self.path);
        Ok(())
    }

    /// Manually trigger a reload
    pub fn reload(&self) -> Result<(), ReloadError> {
        Self::reload_internal(&self.path, &self.config)
    }

    /// Internal reload implementation
    fn reload_internal(path: &Path, config: &ConfigHolder<T>) -> Result<(), ReloadError> {
        let content = fs::read_to_string(path).map_err(|e| ReloadError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let new_config: T =
            serde_yaml::from_str(&content).map_err(|e| ReloadError::ParseError(e.to_string()))?;

        config.update(new_config)
    }

    /// Subscribe to change notifications
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.change_tx.subscribe()
    }

    /// Get the current configuration
    pub fn get(&self) -> Arc<T> {
        self.config.get()
    }

    /// Get configuration holder
    pub fn holder(&self) -> Arc<ConfigHolder<T>> {
        self.config.clone()
    }

    /// Set debounce duration
    pub fn set_debounce(&mut self, duration: Duration) {
        self.debounce = duration;
    }
}

/// Environment-based configuration with reload support
#[derive(Debug, Clone)]
pub struct EnvConfig {
    values: HashMap<String, String>,
    prefix: String,
}

impl EnvConfig {
    /// Create from environment with prefix
    pub fn from_env(prefix: &str) -> Self {
        let prefix = prefix.to_uppercase();
        let values = std::env::vars()
            .filter(|(k, _)| k.starts_with(&prefix))
            .collect();

        Self { values, prefix }
    }

    /// Get a string value
    pub fn get(&self, key: &str) -> Option<&str> {
        let full_key = format!("{}_{}", self.prefix, key.to_uppercase());
        self.values.get(&full_key).map(|s| s.as_str())
    }

    /// Get a string with default
    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_string()
    }

    /// Get a parsed value
    pub fn get_parsed<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.get(key).and_then(|s| s.parse().ok())
    }

    /// Get a parsed value with default
    pub fn get_parsed_or<T: std::str::FromStr>(&self, key: &str, default: T) -> T {
        self.get_parsed(key).unwrap_or(default)
    }

    /// Get a boolean value
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).map(|s| {
            let lower = s.to_lowercase();
            lower == "true" || lower == "1" || lower == "yes"
        })
    }

    /// Get a boolean with default
    pub fn get_bool_or(&self, key: &str, default: bool) -> bool {
        self.get_bool(key).unwrap_or(default)
    }

    /// Reload from environment
    pub fn reload(&mut self) {
        self.values = std::env::vars()
            .filter(|(k, _)| k.starts_with(&self.prefix))
            .collect();
        info!("Environment configuration reloaded");
    }

    /// Get all values
    pub fn all(&self) -> &HashMap<String, String> {
        &self.values
    }
}

impl ReloadableConfig for EnvConfig {
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }

    fn merge(&mut self, other: &Self) {
        self.values.extend(other.values.clone());
    }
}

/// Configuration change event
#[derive(Debug, Clone)]
pub struct ConfigChangeEvent {
    pub timestamp: Instant,
    pub source: ConfigChangeSource,
    pub fields_changed: Vec<String>,
}

/// Source of configuration change
#[derive(Debug, Clone)]
pub enum ConfigChangeSource {
    File(PathBuf),
    Environment,
    Api,
    Default,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, serde::Deserialize)]
    struct TestConfig {
        name: String,
        value: i32,
    }

    impl ReloadableConfig for TestConfig {
        fn validate(&self) -> Result<(), String> {
            if self.value < 0 {
                return Err("Value must be non-negative".to_string());
            }
            Ok(())
        }

        fn merge(&mut self, other: &Self) {
            self.name = other.name.clone();
            self.value = other.value;
        }
    }

    #[test]
    fn test_config_holder() {
        let config = TestConfig {
            name: "test".to_string(),
            value: 42,
        };
        let holder = ConfigHolder::new(config);

        let current = holder.get();
        assert_eq!(current.name, "test");
        assert_eq!(current.value, 42);
    }

    #[test]
    fn test_config_update() {
        let config = TestConfig {
            name: "test".to_string(),
            value: 42,
        };
        let holder = ConfigHolder::new(config);

        let new_config = TestConfig {
            name: "updated".to_string(),
            value: 100,
        };
        holder.update(new_config).unwrap();

        let current = holder.get();
        assert_eq!(current.name, "updated");
        assert_eq!(current.value, 100);
    }

    #[test]
    fn test_config_validation_failure() {
        let config = TestConfig {
            name: "test".to_string(),
            value: 42,
        };
        let holder = ConfigHolder::new(config);

        let invalid_config = TestConfig {
            name: "invalid".to_string(),
            value: -1,
        };
        let result = holder.update(invalid_config);
        assert!(result.is_err());

        // Original config should be unchanged
        let current = holder.get();
        assert_eq!(current.value, 42);
    }

    #[test]
    fn test_config_rollback() {
        let config = TestConfig {
            name: "original".to_string(),
            value: 42,
        };
        let holder = ConfigHolder::new(config);

        let new_config = TestConfig {
            name: "updated".to_string(),
            value: 100,
        };
        holder.update(new_config).unwrap();

        assert!(holder.rollback());

        let current = holder.get();
        assert_eq!(current.name, "original");
    }

    #[test]
    fn test_env_config() {
        std::env::set_var("TEST_FOO", "bar");
        std::env::set_var("TEST_COUNT", "42");
        std::env::set_var("TEST_ENABLED", "true");

        let config = EnvConfig::from_env("TEST");

        assert_eq!(config.get("FOO"), Some("bar"));
        assert_eq!(config.get_parsed::<i32>("COUNT"), Some(42));
        assert_eq!(config.get_bool("ENABLED"), Some(true));
        assert_eq!(config.get("NONEXISTENT"), None);

        // Cleanup
        std::env::remove_var("TEST_FOO");
        std::env::remove_var("TEST_COUNT");
        std::env::remove_var("TEST_ENABLED");
    }
}

//! Graceful degradation patterns for enterprise resilience.
//!
//! Provides:
//! - Graceful shutdown handling
//! - Health-based load shedding
//! - Feature flags for runtime degradation
//! - Fallback mechanisms
//! - Bulkhead isolation
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::graceful::{GracefulShutdown, LoadShedder, FeatureFlags};
//!
//! let shutdown = GracefulShutdown::new();
//! // Handle shutdown signal
//! shutdown.handle_signal().await;
//! ```

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Graceful degradation errors
#[derive(Error, Debug)]
pub enum GracefulError {
    #[error("Service is shutting down")]
    ShuttingDown,

    #[error("Load shedding active - request dropped")]
    LoadShedding,

    #[error("Feature '{0}' is disabled")]
    FeatureDisabled(String),

    #[error("Bulkhead '{0}' is full")]
    BulkheadFull(String),

    #[error("Operation timeout")]
    Timeout,
}

/// Graceful shutdown handler
pub struct GracefulShutdown {
    /// Shutdown flag
    shutdown: AtomicBool,
    /// Shutdown notification channel
    notify: broadcast::Sender<()>,
    /// Active requests counter
    active_requests: AtomicU64,
    /// Shutdown started at
    shutdown_started: RwLock<Option<Instant>>,
    /// Shutdown timeout
    timeout: Duration,
}

impl GracefulShutdown {
    /// Create a new graceful shutdown handler
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(30))
    }

    /// Create with custom timeout
    pub fn with_timeout(timeout: Duration) -> Self {
        let (notify, _) = broadcast::channel(1);
        Self {
            shutdown: AtomicBool::new(false),
            notify,
            active_requests: AtomicU64::new(0),
            shutdown_started: RwLock::new(None),
            timeout,
        }
    }

    /// Check if shutdown is in progress
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Start graceful shutdown
    pub fn initiate(&self) {
        if !self.shutdown.swap(true, Ordering::SeqCst) {
            info!("Graceful shutdown initiated");
            *self.shutdown_started.write() = Some(Instant::now());
            let _ = self.notify.send(());
        }
    }

    /// Subscribe to shutdown notifications
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.notify.subscribe()
    }

    /// Record request start
    pub fn request_start(&self) -> Result<RequestGuard, GracefulError> {
        if self.is_shutting_down() {
            return Err(GracefulError::ShuttingDown);
        }
        self.active_requests.fetch_add(1, Ordering::SeqCst);
        Ok(RequestGuard { handler: self })
    }

    /// Get active request count
    pub fn active_requests(&self) -> u64 {
        self.active_requests.load(Ordering::SeqCst)
    }

    /// Wait for all requests to complete
    pub async fn wait_for_requests(&self) {
        let start = Instant::now();
        loop {
            let count = self.active_requests();
            if count == 0 {
                info!("All requests completed");
                return;
            }

            if start.elapsed() > self.timeout {
                warn!(
                    "Shutdown timeout reached with {} active requests",
                    count
                );
                return;
            }

            debug!("Waiting for {} active requests to complete", count);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Handle shutdown signals (SIGTERM, SIGINT)
    pub async fn handle_signal(&self) {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM");
            let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT");

            tokio::select! {
                _ = sigterm.recv() => {
                    info!("Received SIGTERM");
                }
                _ = sigint.recv() => {
                    info!("Received SIGINT");
                }
            }
        }

        #[cfg(windows)]
        {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to register Ctrl+C handler");
            info!("Received Ctrl+C");
        }

        self.initiate();
        self.wait_for_requests().await;
    }
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard that decrements request count on drop
pub struct RequestGuard<'a> {
    handler: &'a GracefulShutdown,
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        self.handler.active_requests.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Load shedding configuration
#[derive(Debug, Clone)]
pub struct LoadShedderConfig {
    /// Enable load shedding
    pub enabled: bool,
    /// CPU threshold (percentage, 0-100)
    pub cpu_threshold: u32,
    /// Memory threshold (percentage, 0-100)
    pub memory_threshold: u32,
    /// Max concurrent requests
    pub max_concurrent: u64,
    /// Shed percentage when overloaded (0-100)
    pub shed_percentage: u32,
}

impl Default for LoadShedderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cpu_threshold: 80,
            memory_threshold: 85,
            max_concurrent: 1000,
            shed_percentage: 50,
        }
    }
}

/// Load shedder for dropping requests under high load
pub struct LoadShedder {
    config: LoadShedderConfig,
    current_requests: AtomicU64,
    dropped_requests: AtomicU64,
    overloaded: AtomicBool,
}

impl LoadShedder {
    /// Create a new load shedder
    pub fn new(config: LoadShedderConfig) -> Self {
        Self {
            config,
            current_requests: AtomicU64::new(0),
            dropped_requests: AtomicU64::new(0),
            overloaded: AtomicBool::new(false),
        }
    }

    /// Check if request should be accepted
    pub fn should_accept(&self) -> bool {
        if !self.config.enabled {
            return true;
        }

        let current = self.current_requests.load(Ordering::SeqCst);

        // Check concurrent limit
        if current >= self.config.max_concurrent {
            self.dropped_requests.fetch_add(1, Ordering::SeqCst);
            warn!("Load shedding: concurrent limit reached ({})", current);
            return false;
        }

        // Check if in overloaded state
        if self.overloaded.load(Ordering::SeqCst) {
            // Probabilistic shedding based on shed_percentage
            let should_shed = rand_simple() < (self.config.shed_percentage as f64 / 100.0);
            if should_shed {
                self.dropped_requests.fetch_add(1, Ordering::SeqCst);
                debug!("Load shedding: probabilistic drop");
                return false;
            }
        }

        true
    }

    /// Acquire a request slot
    pub fn acquire(&self) -> Result<LoadShedGuard, GracefulError> {
        if !self.should_accept() {
            return Err(GracefulError::LoadShedding);
        }
        self.current_requests.fetch_add(1, Ordering::SeqCst);
        Ok(LoadShedGuard { shedder: self })
    }

    /// Set overloaded state
    pub fn set_overloaded(&self, overloaded: bool) {
        self.overloaded.store(overloaded, Ordering::SeqCst);
        if overloaded {
            warn!("System entering overloaded state - load shedding enabled");
        } else {
            info!("System exiting overloaded state");
        }
    }

    /// Get statistics
    pub fn stats(&self) -> LoadShedderStats {
        LoadShedderStats {
            current_requests: self.current_requests.load(Ordering::SeqCst),
            dropped_requests: self.dropped_requests.load(Ordering::SeqCst),
            is_overloaded: self.overloaded.load(Ordering::SeqCst),
        }
    }
}

/// Load shedder statistics
#[derive(Debug, Clone)]
pub struct LoadShedderStats {
    pub current_requests: u64,
    pub dropped_requests: u64,
    pub is_overloaded: bool,
}

/// Guard that releases request slot on drop
pub struct LoadShedGuard<'a> {
    shedder: &'a LoadShedder,
}

impl Drop for LoadShedGuard<'_> {
    fn drop(&mut self) {
        self.shedder.current_requests.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Feature flags for runtime feature toggling
#[derive(Debug, Clone)]
pub struct FeatureFlags {
    flags: Arc<RwLock<HashMap<String, bool>>>,
    defaults: HashMap<String, bool>,
}

impl FeatureFlags {
    /// Create new feature flags
    pub fn new() -> Self {
        Self {
            flags: Arc::new(RwLock::new(HashMap::new())),
            defaults: HashMap::new(),
        }
    }

    /// Register a feature with default value
    pub fn register(&mut self, name: &str, default: bool) {
        self.defaults.insert(name.to_string(), default);
    }

    /// Check if feature is enabled
    pub fn is_enabled(&self, name: &str) -> bool {
        let flags = self.flags.read();
        flags
            .get(name)
            .copied()
            .or_else(|| self.defaults.get(name).copied())
            .unwrap_or(false)
    }

    /// Enable a feature
    pub fn enable(&self, name: &str) {
        let mut flags = self.flags.write();
        flags.insert(name.to_string(), true);
        info!("Feature '{}' enabled", name);
    }

    /// Disable a feature
    pub fn disable(&self, name: &str) {
        let mut flags = self.flags.write();
        flags.insert(name.to_string(), false);
        info!("Feature '{}' disabled", name);
    }

    /// Set feature state
    pub fn set(&self, name: &str, enabled: bool) {
        let mut flags = self.flags.write();
        flags.insert(name.to_string(), enabled);
        info!("Feature '{}' set to {}", name, enabled);
    }

    /// Get all flags
    pub fn all(&self) -> HashMap<String, bool> {
        let flags = self.flags.read();
        let mut all = self.defaults.clone();
        all.extend(flags.iter().map(|(k, v)| (k.clone(), *v)));
        all
    }

    /// Execute closure if feature is enabled
    pub fn with_feature<F, T>(&self, name: &str, f: F) -> Result<T, GracefulError>
    where
        F: FnOnce() -> T,
    {
        if self.is_enabled(name) {
            Ok(f())
        } else {
            Err(GracefulError::FeatureDisabled(name.to_string()))
        }
    }
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self::new()
    }
}

/// Bulkhead for isolating resources
#[derive(Debug)]
pub struct Bulkhead {
    name: String,
    max_concurrent: u64,
    current: AtomicU64,
    rejected: AtomicU64,
}

impl Bulkhead {
    /// Create a new bulkhead
    pub fn new(name: impl Into<String>, max_concurrent: u64) -> Self {
        Self {
            name: name.into(),
            max_concurrent,
            current: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    /// Try to acquire a permit
    pub fn try_acquire(&self) -> Result<BulkheadPermit, GracefulError> {
        let current = self.current.fetch_add(1, Ordering::SeqCst);
        if current >= self.max_concurrent {
            self.current.fetch_sub(1, Ordering::SeqCst);
            self.rejected.fetch_add(1, Ordering::SeqCst);
            warn!(
                "Bulkhead '{}' full: {}/{}",
                self.name, current, self.max_concurrent
            );
            return Err(GracefulError::BulkheadFull(self.name.clone()));
        }
        debug!(
            "Bulkhead '{}' acquired: {}/{}",
            self.name,
            current + 1,
            self.max_concurrent
        );
        Ok(BulkheadPermit { bulkhead: self })
    }

    /// Get current usage
    pub fn current(&self) -> u64 {
        self.current.load(Ordering::SeqCst)
    }

    /// Get rejection count
    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::SeqCst)
    }
}

/// Permit for bulkhead access
pub struct BulkheadPermit<'a> {
    bulkhead: &'a Bulkhead,
}

impl Drop for BulkheadPermit<'_> {
    fn drop(&mut self) {
        self.bulkhead.current.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Fallback handler for degraded operations
pub struct Fallback<T> {
    primary: Arc<dyn Fn() -> T + Send + Sync>,
    fallback: Arc<dyn Fn() -> T + Send + Sync>,
    use_fallback: AtomicBool,
}

impl<T> Fallback<T> {
    /// Create a new fallback handler
    pub fn new<P, F>(primary: P, fallback: F) -> Self
    where
        P: Fn() -> T + Send + Sync + 'static,
        F: Fn() -> T + Send + Sync + 'static,
    {
        Self {
            primary: Arc::new(primary),
            fallback: Arc::new(fallback),
            use_fallback: AtomicBool::new(false),
        }
    }

    /// Execute with fallback
    pub fn execute(&self) -> T {
        if self.use_fallback.load(Ordering::SeqCst) {
            (self.fallback)()
        } else {
            (self.primary)()
        }
    }

    /// Enable fallback mode
    pub fn enable_fallback(&self) {
        self.use_fallback.store(true, Ordering::SeqCst);
        warn!("Fallback mode enabled");
    }

    /// Disable fallback mode
    pub fn disable_fallback(&self) {
        self.use_fallback.store(false, Ordering::SeqCst);
        info!("Fallback mode disabled");
    }

    /// Check if in fallback mode
    pub fn is_fallback(&self) -> bool {
        self.use_fallback.load(Ordering::SeqCst)
    }
}

// Simple random number generator (no external dependency)
fn rand_simple() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos as f64) / 1_000_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graceful_shutdown() {
        let shutdown = GracefulShutdown::new();
        assert!(!shutdown.is_shutting_down());

        shutdown.initiate();
        assert!(shutdown.is_shutting_down());
    }

    #[test]
    fn test_request_guard() {
        let shutdown = GracefulShutdown::new();
        assert_eq!(shutdown.active_requests(), 0);

        {
            let _guard = shutdown.request_start().unwrap();
            assert_eq!(shutdown.active_requests(), 1);
        }

        assert_eq!(shutdown.active_requests(), 0);
    }

    #[test]
    fn test_request_rejected_during_shutdown() {
        let shutdown = GracefulShutdown::new();
        shutdown.initiate();

        let result = shutdown.request_start();
        assert!(result.is_err());
    }

    #[test]
    fn test_load_shedder() {
        let config = LoadShedderConfig {
            enabled: true,
            max_concurrent: 2,
            ..Default::default()
        };
        let shedder = LoadShedder::new(config);

        let _guard1 = shedder.acquire().unwrap();
        let _guard2 = shedder.acquire().unwrap();

        // Third should fail
        let result = shedder.acquire();
        assert!(result.is_err());
    }

    #[test]
    fn test_feature_flags() {
        let mut flags = FeatureFlags::new();
        flags.register("feature1", true);
        flags.register("feature2", false);

        assert!(flags.is_enabled("feature1"));
        assert!(!flags.is_enabled("feature2"));
        assert!(!flags.is_enabled("unknown"));

        flags.disable("feature1");
        assert!(!flags.is_enabled("feature1"));

        flags.enable("feature2");
        assert!(flags.is_enabled("feature2"));
    }

    #[test]
    fn test_bulkhead() {
        let bulkhead = Bulkhead::new("test", 2);

        let _permit1 = bulkhead.try_acquire().unwrap();
        let _permit2 = bulkhead.try_acquire().unwrap();

        // Third should fail
        let result = bulkhead.try_acquire();
        assert!(result.is_err());

        assert_eq!(bulkhead.rejected(), 1);
    }

    #[test]
    fn test_fallback() {
        let fallback = Fallback::new(|| "primary", || "fallback");

        assert_eq!(fallback.execute(), "primary");
        assert!(!fallback.is_fallback());

        fallback.enable_fallback();
        assert_eq!(fallback.execute(), "fallback");
        assert!(fallback.is_fallback());

        fallback.disable_fallback();
        assert_eq!(fallback.execute(), "primary");
    }
}

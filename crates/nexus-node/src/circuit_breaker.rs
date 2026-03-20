//! Circuit breaker pattern implementation for enterprise resilience.
//!
//! Provides protection against cascading failures by:
//! - Detecting failure patterns
//! - Opening circuit to prevent further failures
//! - Allowing periodic retries to detect recovery
//! - Automatic recovery when service is healthy
//!
//! # States
//!
//! - **Closed**: Normal operation, requests pass through
//! - **Open**: Circuit tripped, requests fail fast
//! - **Half-Open**: Testing if service has recovered
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
//!
//! let config = CircuitBreakerConfig::default();
//! let breaker = CircuitBreaker::new("containerd", config);
//!
//! // Execute with circuit breaker protection
//! let result = breaker.execute(|| async {
//!     // Call external service
//! }).await;
//! ```

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Circuit breaker errors
#[derive(Error, Debug, Clone)]
pub enum CircuitBreakerError {
    #[error("Circuit breaker is open for service '{service}'")]
    CircuitOpen { service: String },

    #[error("Operation timed out after {0:?}")]
    Timeout(Duration),

    #[error("Operation failed: {0}")]
    OperationFailed(String),
}

/// Circuit breaker state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CircuitState {
    /// Normal operation
    Closed,
    /// Circuit tripped, failing fast
    Open,
    /// Testing recovery
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

/// Circuit breaker configuration
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening circuit
    pub failure_threshold: u32,
    /// Time to wait before attempting recovery (half-open state)
    pub recovery_timeout: Duration,
    /// Number of successful calls to close circuit
    pub success_threshold: u32,
    /// Rolling window for failure counting
    pub failure_window: Duration,
    /// Operation timeout
    pub operation_timeout: Duration,
    /// Enable circuit breaker
    pub enabled: bool,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
            success_threshold: 3,
            failure_window: Duration::from_secs(60),
            operation_timeout: Duration::from_secs(30),
            enabled: true,
        }
    }
}

impl CircuitBreakerConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("CIRCUIT_BREAKER_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }

        if let Ok(threshold) = std::env::var("CIRCUIT_BREAKER_FAILURE_THRESHOLD") {
            config.failure_threshold = threshold.parse().unwrap_or(5);
        }

        if let Ok(timeout) = std::env::var("CIRCUIT_BREAKER_RECOVERY_TIMEOUT") {
            if let Ok(secs) = timeout.parse::<u64>() {
                config.recovery_timeout = Duration::from_secs(secs);
            }
        }

        if let Ok(threshold) = std::env::var("CIRCUIT_BREAKER_SUCCESS_THRESHOLD") {
            config.success_threshold = threshold.parse().unwrap_or(3);
        }

        config
    }

    /// Create a conservative configuration for critical services
    pub fn critical() -> Self {
        Self {
            failure_threshold: 3,
            recovery_timeout: Duration::from_secs(60),
            success_threshold: 5,
            failure_window: Duration::from_secs(120),
            operation_timeout: Duration::from_secs(60),
            enabled: true,
        }
    }

    /// Create an aggressive configuration for non-critical services
    pub fn lenient() -> Self {
        Self {
            failure_threshold: 10,
            recovery_timeout: Duration::from_secs(15),
            success_threshold: 2,
            failure_window: Duration::from_secs(30),
            operation_timeout: Duration::from_secs(15),
            enabled: true,
        }
    }
}

/// Internal state for a circuit breaker
#[derive(Debug)]
struct CircuitBreakerState {
    /// Current circuit state
    state: CircuitState,
    /// Failure timestamps within window
    failures: Vec<Instant>,
    /// Consecutive successes in half-open state
    half_open_successes: u32,
    /// Time when circuit was opened
    opened_at: Option<Instant>,
    /// Last state transition time
    last_transition: Instant,
    /// Total failure count
    total_failures: u64,
    /// Total success count
    total_successes: u64,
}

impl Default for CircuitBreakerState {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            failures: Vec::new(),
            half_open_successes: 0,
            opened_at: None,
            last_transition: Instant::now(),
            total_failures: 0,
            total_successes: 0,
        }
    }
}

/// Circuit breaker for a single service
pub struct CircuitBreaker {
    /// Service name
    name: String,
    /// Configuration
    config: CircuitBreakerConfig,
    /// Internal state
    state: Arc<RwLock<CircuitBreakerState>>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker
    pub fn new(name: impl Into<String>, config: CircuitBreakerConfig) -> Self {
        Self {
            name: name.into(),
            config,
            state: Arc::new(RwLock::new(CircuitBreakerState::default())),
        }
    }

    /// Get the current circuit state
    pub fn current_state(&self) -> CircuitState {
        let mut state = self.state.write();
        self.maybe_transition(&mut state);
        state.state
    }

    /// Get circuit breaker statistics
    pub fn stats(&self) -> CircuitBreakerStats {
        let state = self.state.read();
        CircuitBreakerStats {
            state: state.state,
            total_failures: state.total_failures,
            total_successes: state.total_successes,
            recent_failures: state.failures.len() as u32,
            opened_at: state.opened_at,
            last_transition: state.last_transition,
        }
    }

    /// Check if circuit allows requests
    pub fn allows_request(&self) -> bool {
        if !self.config.enabled {
            return true;
        }

        let mut state = self.state.write();
        self.maybe_transition(&mut state);

        match state.state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => true,
            CircuitState::Open => false,
        }
    }

    /// Record a successful operation
    pub fn record_success(&self) {
        if !self.config.enabled {
            return;
        }

        let mut state = self.state.write();
        state.total_successes += 1;

        match state.state {
            CircuitState::HalfOpen => {
                state.half_open_successes += 1;
                debug!(
                    "Circuit '{}' half-open success: {}/{}",
                    self.name, state.half_open_successes, self.config.success_threshold
                );

                if state.half_open_successes >= self.config.success_threshold {
                    info!("Circuit '{}' closing after recovery", self.name);
                    state.state = CircuitState::Closed;
                    state.failures.clear();
                    state.half_open_successes = 0;
                    state.opened_at = None;
                    state.last_transition = Instant::now();
                }
            }
            CircuitState::Closed => {
                // Remove old failures outside window
                let window_start = Instant::now() - self.config.failure_window;
                state.failures.retain(|t| *t > window_start);
            }
            CircuitState::Open => {}
        }
    }

    /// Record a failed operation
    pub fn record_failure(&self, error: &str) {
        if !self.config.enabled {
            return;
        }

        let mut state = self.state.write();
        state.total_failures += 1;

        let now = Instant::now();

        match state.state {
            CircuitState::Closed => {
                state.failures.push(now);

                // Remove old failures outside window
                let window_start = now - self.config.failure_window;
                state.failures.retain(|t| *t > window_start);

                let failure_count = state.failures.len() as u32;
                debug!(
                    "Circuit '{}' failure {}/{}: {}",
                    self.name, failure_count, self.config.failure_threshold, error
                );

                if failure_count >= self.config.failure_threshold {
                    warn!(
                        "Circuit '{}' opening after {} failures",
                        self.name, failure_count
                    );
                    state.state = CircuitState::Open;
                    state.opened_at = Some(now);
                    state.last_transition = now;
                }
            }
            CircuitState::HalfOpen => {
                warn!(
                    "Circuit '{}' reopening after failure in half-open state: {}",
                    self.name, error
                );
                state.state = CircuitState::Open;
                state.opened_at = Some(now);
                state.half_open_successes = 0;
                state.last_transition = now;
            }
            CircuitState::Open => {}
        }
    }

    /// Check if state transition is needed
    fn maybe_transition(&self, state: &mut CircuitBreakerState) {
        if state.state == CircuitState::Open {
            if let Some(opened_at) = state.opened_at {
                if opened_at.elapsed() >= self.config.recovery_timeout {
                    info!(
                        "Circuit '{}' transitioning to half-open for recovery test",
                        self.name
                    );
                    state.state = CircuitState::HalfOpen;
                    state.half_open_successes = 0;
                    state.last_transition = Instant::now();
                }
            }
        }
    }

    /// Execute an operation with circuit breaker protection
    pub async fn execute<F, Fut, T, E>(&self, operation: F) -> Result<T, CircuitBreakerError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        // Check if circuit allows request
        if !self.allows_request() {
            return Err(CircuitBreakerError::CircuitOpen {
                service: self.name.clone(),
            });
        }

        // Execute with timeout
        let result = tokio::time::timeout(self.config.operation_timeout, operation()).await;

        match result {
            Ok(Ok(value)) => {
                self.record_success();
                Ok(value)
            }
            Ok(Err(e)) => {
                let error_msg = e.to_string();
                self.record_failure(&error_msg);
                Err(CircuitBreakerError::OperationFailed(error_msg))
            }
            Err(_) => {
                self.record_failure("timeout");
                Err(CircuitBreakerError::Timeout(self.config.operation_timeout))
            }
        }
    }

    /// Force the circuit to open
    pub fn force_open(&self) {
        let mut state = self.state.write();
        state.state = CircuitState::Open;
        state.opened_at = Some(Instant::now());
        state.last_transition = Instant::now();
        warn!("Circuit '{}' force opened", self.name);
    }

    /// Force the circuit to close
    pub fn force_close(&self) {
        let mut state = self.state.write();
        state.state = CircuitState::Closed;
        state.failures.clear();
        state.half_open_successes = 0;
        state.opened_at = None;
        state.last_transition = Instant::now();
        info!("Circuit '{}' force closed", self.name);
    }
}

/// Statistics for a circuit breaker
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub state: CircuitState,
    pub total_failures: u64,
    pub total_successes: u64,
    pub recent_failures: u32,
    pub opened_at: Option<Instant>,
    pub last_transition: Instant,
}

/// Registry of circuit breakers
pub struct CircuitBreakerRegistry {
    breakers: Arc<RwLock<HashMap<String, Arc<CircuitBreaker>>>>,
    default_config: CircuitBreakerConfig,
}

impl CircuitBreakerRegistry {
    /// Create a new registry
    pub fn new(default_config: CircuitBreakerConfig) -> Self {
        Self {
            breakers: Arc::new(RwLock::new(HashMap::new())),
            default_config,
        }
    }

    /// Get or create a circuit breaker for a service
    pub fn get(&self, name: &str) -> Arc<CircuitBreaker> {
        let breakers = self.breakers.read();
        if let Some(breaker) = breakers.get(name) {
            return breaker.clone();
        }
        drop(breakers);

        let mut breakers = self.breakers.write();
        // Double-check after acquiring write lock
        if let Some(breaker) = breakers.get(name) {
            return breaker.clone();
        }

        let breaker = Arc::new(CircuitBreaker::new(name, self.default_config.clone()));
        breakers.insert(name.to_string(), breaker.clone());
        breaker
    }

    /// Get or create a circuit breaker with custom config
    pub fn get_with_config(&self, name: &str, config: CircuitBreakerConfig) -> Arc<CircuitBreaker> {
        let breakers = self.breakers.read();
        if let Some(breaker) = breakers.get(name) {
            return breaker.clone();
        }
        drop(breakers);

        let mut breakers = self.breakers.write();
        if let Some(breaker) = breakers.get(name) {
            return breaker.clone();
        }

        let breaker = Arc::new(CircuitBreaker::new(name, config));
        breakers.insert(name.to_string(), breaker.clone());
        breaker
    }

    /// Get all circuit breaker stats
    pub fn all_stats(&self) -> HashMap<String, CircuitBreakerStats> {
        let breakers = self.breakers.read();
        breakers.iter().map(|(name, breaker)| (name.clone(), breaker.stats())).collect()
    }

    /// Force open all circuit breakers
    pub fn force_open_all(&self) {
        let breakers = self.breakers.read();
        for (_, breaker) in breakers.iter() {
            breaker.force_open();
        }
    }

    /// Force close all circuit breakers
    pub fn force_close_all(&self) {
        let breakers = self.breakers.read();
        for (_, breaker) in breakers.iter() {
            breaker.force_close();
        }
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let breaker = CircuitBreaker::new("test", CircuitBreakerConfig::default());
        assert_eq!(breaker.current_state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_opens_after_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            failure_window: Duration::from_secs(60),
            ..Default::default()
        };
        let breaker = CircuitBreaker::new("test", config);

        // Record failures
        breaker.record_failure("error 1");
        breaker.record_failure("error 2");
        assert_eq!(breaker.current_state(), CircuitState::Closed);

        breaker.record_failure("error 3");
        assert_eq!(breaker.current_state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_allows_request_when_closed() {
        let breaker = CircuitBreaker::new("test", CircuitBreakerConfig::default());
        assert!(breaker.allows_request());
    }

    #[test]
    fn test_circuit_denies_request_when_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            recovery_timeout: Duration::from_secs(3600), // Long timeout
            ..Default::default()
        };
        let breaker = CircuitBreaker::new("test", config);

        breaker.record_failure("error");
        assert!(!breaker.allows_request());
    }

    #[test]
    fn test_success_in_closed_state() {
        let breaker = CircuitBreaker::new("test", CircuitBreakerConfig::default());

        breaker.record_success();
        let stats = breaker.stats();
        assert_eq!(stats.total_successes, 1);
        assert_eq!(stats.state, CircuitState::Closed);
    }

    #[test]
    fn test_registry_creates_breakers() {
        let registry = CircuitBreakerRegistry::default();

        let breaker1 = registry.get("service1");
        let breaker2 = registry.get("service2");
        let breaker1_again = registry.get("service1");

        // Should return the same instance
        assert!(Arc::ptr_eq(&breaker1, &breaker1_again));
        assert!(!Arc::ptr_eq(&breaker1, &breaker2));
    }

    #[tokio::test]
    async fn test_execute_success() {
        let breaker = CircuitBreaker::new("test", CircuitBreakerConfig::default());

        let result: Result<i32, CircuitBreakerError> =
            breaker.execute(|| async { Ok::<_, std::io::Error>(42) }).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_execute_failure() {
        let breaker = CircuitBreaker::new("test", CircuitBreakerConfig::default());

        let result: Result<i32, CircuitBreakerError> = breaker
            .execute(|| async {
                Err::<i32, _>(std::io::Error::new(std::io::ErrorKind::Other, "test error"))
            })
            .await;

        assert!(result.is_err());
    }
}

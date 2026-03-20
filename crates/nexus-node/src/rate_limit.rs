//! Rate limiting middleware for enterprise gRPC services.
//!
//! Provides configurable rate limiting with multiple strategies:
//! - Global rate limiting
//! - Per-client rate limiting (by IP or API key)
//! - Per-method rate limiting
//! - Burst handling with token bucket algorithm
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::rate_limit::{RateLimitConfig, RateLimitLayer};
//!
//! let config = RateLimitConfig::default();
//! let layer = RateLimitLayer::new(config);
//! ```

use governor::{
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, warn};

/// Rate limiting errors
#[derive(Error, Debug)]
pub enum RateLimitError {
    #[error("Rate limit exceeded: {0} requests per {1:?}")]
    LimitExceeded(u32, Duration),

    #[error("Too many concurrent requests")]
    ConcurrencyLimitExceeded,

    #[error("Client temporarily blocked")]
    ClientBlocked,
}

/// Rate limit configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Enable rate limiting
    pub enabled: bool,
    /// Global requests per second
    pub global_rps: u32,
    /// Burst size (token bucket)
    pub burst_size: u32,
    /// Per-client requests per second
    pub per_client_rps: u32,
    /// Per-client burst size
    pub per_client_burst: u32,
    /// Maximum concurrent requests per client
    pub max_concurrent_per_client: u32,
    /// Methods exempt from rate limiting
    pub exempt_methods: Vec<String>,
    /// Custom limits per method
    pub method_limits: HashMap<String, MethodLimit>,
    /// Block duration for repeated violations
    pub block_duration: Duration,
    /// Number of violations before blocking
    pub violations_before_block: u32,
}

/// Rate limit for a specific method
#[derive(Debug, Clone)]
pub struct MethodLimit {
    /// Requests per second
    pub rps: u32,
    /// Burst size
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            global_rps: 10000,
            burst_size: 1000,
            per_client_rps: 100,
            per_client_burst: 50,
            max_concurrent_per_client: 20,
            exempt_methods: vec![
                "/nexus.node.v1.NodeService/HealthCheck".to_string(),
                "/grpc.health.v1.Health/Check".to_string(),
            ],
            method_limits: HashMap::new(),
            block_duration: Duration::from_secs(60),
            violations_before_block: 10,
        }
    }
}

impl RateLimitConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("RATE_LIMIT_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }

        if let Ok(rps) = std::env::var("RATE_LIMIT_GLOBAL_RPS") {
            config.global_rps = rps.parse().unwrap_or(10000);
        }

        if let Ok(burst) = std::env::var("RATE_LIMIT_BURST_SIZE") {
            config.burst_size = burst.parse().unwrap_or(1000);
        }

        if let Ok(rps) = std::env::var("RATE_LIMIT_PER_CLIENT_RPS") {
            config.per_client_rps = rps.parse().unwrap_or(100);
        }

        if let Ok(concurrent) = std::env::var("RATE_LIMIT_MAX_CONCURRENT") {
            config.max_concurrent_per_client = concurrent.parse().unwrap_or(20);
        }

        config
    }

    /// Add method-specific rate limit
    pub fn set_method_limit(&mut self, method: &str, rps: u32, burst: u32) {
        self.method_limits.insert(method.to_string(), MethodLimit { rps, burst });
    }
}

/// Client state for rate limiting
#[derive(Debug)]
struct ClientState {
    /// Rate limiter for this client
    limiter: RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>,
    /// Current concurrent requests
    concurrent: u32,
    /// Violation count
    violations: u32,
    /// Last violation time
    last_violation: Option<Instant>,
    /// Block expiry
    blocked_until: Option<Instant>,
}

impl ClientState {
    fn new(quota: Quota) -> Self {
        Self {
            limiter: RateLimiter::direct(quota),
            concurrent: 0,
            violations: 0,
            last_violation: None,
            blocked_until: None,
        }
    }

    fn is_blocked(&self) -> bool {
        if let Some(blocked_until) = self.blocked_until {
            Instant::now() < blocked_until
        } else {
            false
        }
    }

    fn record_violation(&mut self, config: &RateLimitConfig) {
        self.violations += 1;
        self.last_violation = Some(Instant::now());

        if self.violations >= config.violations_before_block {
            self.blocked_until = Some(Instant::now() + config.block_duration);
            warn!(
                "Client blocked for {:?} after {} violations",
                config.block_duration, self.violations
            );
        }
    }

    fn reset_if_expired(&mut self) {
        // Reset violations if last violation was more than 5 minutes ago
        if let Some(last) = self.last_violation {
            if last.elapsed() > Duration::from_secs(300) {
                self.violations = 0;
                self.last_violation = None;
                self.blocked_until = None;
            }
        }
    }
}

/// Rate limiter service
pub struct RateLimiter_ {
    config: Arc<RateLimitConfig>,
    /// Global rate limiter
    global: RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>,
    /// Per-client limiters
    clients: Arc<RwLock<HashMap<String, ClientState>>>,
    /// Per-method limiters
    methods: Arc<
        RwLock<HashMap<String, RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>>>,
    >,
}

impl RateLimiter_ {
    /// Create a new rate limiter
    pub fn new(config: RateLimitConfig) -> Self {
        let global_quota = Quota::per_second(
            NonZeroU32::new(config.global_rps).unwrap_or(NonZeroU32::new(10000).unwrap()),
        )
        .allow_burst(NonZeroU32::new(config.burst_size).unwrap_or(NonZeroU32::new(1000).unwrap()));

        let mut method_limiters = HashMap::new();
        for (method, limit) in &config.method_limits {
            let quota = Quota::per_second(
                NonZeroU32::new(limit.rps).unwrap_or(NonZeroU32::new(100).unwrap()),
            )
            .allow_burst(NonZeroU32::new(limit.burst).unwrap_or(NonZeroU32::new(50).unwrap()));
            method_limiters.insert(method.clone(), RateLimiter::direct(quota));
        }

        Self {
            config: Arc::new(config),
            global: RateLimiter::direct(global_quota),
            clients: Arc::new(RwLock::new(HashMap::new())),
            methods: Arc::new(RwLock::new(method_limiters)),
        }
    }

    /// Check if a request should be allowed
    pub fn check_request(
        &self,
        client_id: &str,
        method: &str,
    ) -> Result<RateLimitGuard, RateLimitError> {
        // Check if rate limiting is enabled
        if !self.config.enabled {
            return Ok(RateLimitGuard::new(
                self.clients.clone(),
                client_id.to_string(),
            ));
        }

        // Check if method is exempt
        if self.config.exempt_methods.contains(&method.to_string()) {
            return Ok(RateLimitGuard::new(
                self.clients.clone(),
                client_id.to_string(),
            ));
        }

        // Check global rate limit
        if self.global.check().is_err() {
            warn!("Global rate limit exceeded");
            return Err(RateLimitError::LimitExceeded(
                self.config.global_rps,
                Duration::from_secs(1),
            ));
        }

        // Check method-specific limit
        {
            let methods = self.methods.read();
            if let Some(limiter) = methods.get(method) {
                if limiter.check().is_err() {
                    let limit = self.config.method_limits.get(method).map(|l| l.rps).unwrap_or(100);
                    warn!("Method rate limit exceeded for {}", method);
                    return Err(RateLimitError::LimitExceeded(limit, Duration::from_secs(1)));
                }
            }
        }

        // Check per-client limit
        let mut clients = self.clients.write();
        let client = clients.entry(client_id.to_string()).or_insert_with(|| {
            let quota = Quota::per_second(
                NonZeroU32::new(self.config.per_client_rps)
                    .unwrap_or(NonZeroU32::new(100).unwrap()),
            )
            .allow_burst(
                NonZeroU32::new(self.config.per_client_burst)
                    .unwrap_or(NonZeroU32::new(50).unwrap()),
            );
            ClientState::new(quota)
        });

        // Reset violations if expired
        client.reset_if_expired();

        // Check if client is blocked
        if client.is_blocked() {
            return Err(RateLimitError::ClientBlocked);
        }

        // Check client rate limit
        if client.limiter.check().is_err() {
            client.record_violation(&self.config);
            return Err(RateLimitError::LimitExceeded(
                self.config.per_client_rps,
                Duration::from_secs(1),
            ));
        }

        // Check concurrent request limit
        if client.concurrent >= self.config.max_concurrent_per_client {
            client.record_violation(&self.config);
            return Err(RateLimitError::ConcurrencyLimitExceeded);
        }

        // Increment concurrent count
        client.concurrent += 1;
        debug!(
            "Client {} concurrent requests: {}",
            client_id, client.concurrent
        );

        Ok(RateLimitGuard::new(
            self.clients.clone(),
            client_id.to_string(),
        ))
    }

    /// Get current client state
    pub fn get_client_stats(&self, client_id: &str) -> Option<ClientStats> {
        let clients = self.clients.read();
        clients.get(client_id).map(|c| ClientStats {
            concurrent_requests: c.concurrent,
            violations: c.violations,
            is_blocked: c.is_blocked(),
        })
    }

    /// Cleanup expired client states
    pub fn cleanup(&self) {
        let mut clients = self.clients.write();
        clients.retain(|_, state| {
            // Keep if active recently (within last hour)
            state.last_violation.map_or(true, |t| t.elapsed() < Duration::from_secs(3600))
        });
    }
}

/// Statistics for a client
#[derive(Debug, Clone)]
pub struct ClientStats {
    pub concurrent_requests: u32,
    pub violations: u32,
    pub is_blocked: bool,
}

/// Guard that decrements concurrent count on drop
pub struct RateLimitGuard {
    clients: Arc<RwLock<HashMap<String, ClientState>>>,
    client_id: String,
    released: bool,
}

impl RateLimitGuard {
    fn new(clients: Arc<RwLock<HashMap<String, ClientState>>>, client_id: String) -> Self {
        Self {
            clients,
            client_id,
            released: false,
        }
    }

    /// Manually release the guard
    pub fn release(&mut self) {
        if !self.released {
            self.released = true;
            let mut clients = self.clients.write();
            if let Some(client) = clients.get_mut(&self.client_id) {
                client.concurrent = client.concurrent.saturating_sub(1);
            }
        }
    }
}

impl Drop for RateLimitGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// Tower layer for rate limiting
pub mod layer {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::body::BoxBody;
    use tower::{Layer, Service};

    /// Rate limit layer
    #[derive(Clone)]
    pub struct RateLimitLayer {
        limiter: Arc<RateLimiter_>,
    }

    impl RateLimitLayer {
        pub fn new(config: RateLimitConfig) -> Self {
            Self {
                limiter: Arc::new(RateLimiter_::new(config)),
            }
        }
    }

    impl<S> Layer<S> for RateLimitLayer {
        type Service = RateLimitService<S>;

        fn layer(&self, service: S) -> Self::Service {
            RateLimitService {
                inner: service,
                limiter: self.limiter.clone(),
            }
        }
    }

    /// Rate limiting service wrapper
    #[derive(Clone)]
    pub struct RateLimitService<S> {
        inner: S,
        limiter: Arc<RateLimiter_>,
    }

    impl<S, B> Service<http::Request<B>> for RateLimitService<S>
    where
        S: Service<http::Request<B>, Response = http::Response<BoxBody>> + Clone + Send + 'static,
        S::Future: Send + 'static,
        B: Send + 'static,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx)
        }

        fn call(&mut self, request: http::Request<B>) -> Self::Future {
            let limiter = self.limiter.clone();
            let mut inner = self.inner.clone();
            let method = request.uri().path().to_string();

            // Extract client identifier
            let client_id = request
                .headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.split(',').next().unwrap_or("unknown").trim().to_string())
                .or_else(|| {
                    request
                        .headers()
                        .get("x-api-key")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| format!("apikey:{}", &s[..8.min(s.len())]))
                })
                .unwrap_or_else(|| "anonymous".to_string());

            Box::pin(async move {
                match limiter.check_request(&client_id, &method) {
                    Ok(_guard) => {
                        // Guard will be dropped after request completes
                        inner.call(request).await
                    }
                    Err(e) => {
                        warn!("Rate limit rejected request from {}: {:?}", client_id, e);
                        let (status, retry_after) = match e {
                            RateLimitError::LimitExceeded(_, _) => {
                                (http::StatusCode::TOO_MANY_REQUESTS, Some(1))
                            }
                            RateLimitError::ConcurrencyLimitExceeded => {
                                (http::StatusCode::TOO_MANY_REQUESTS, Some(5))
                            }
                            RateLimitError::ClientBlocked => {
                                (http::StatusCode::TOO_MANY_REQUESTS, Some(60))
                            }
                        };

                        let mut response = http::Response::builder().status(status);

                        if let Some(retry) = retry_after {
                            response = response.header("Retry-After", retry.to_string());
                        }

                        let response = response.body(tonic::body::empty_body()).unwrap();
                        Ok(response)
                    }
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_config_default() {
        let config = RateLimitConfig::default();
        assert!(config.enabled);
        assert_eq!(config.global_rps, 10000);
        assert_eq!(config.per_client_rps, 100);
    }

    #[test]
    fn test_rate_limiter_allows_requests() {
        let config = RateLimitConfig {
            enabled: true,
            global_rps: 1000,
            burst_size: 100,
            per_client_rps: 10,
            per_client_burst: 5,
            max_concurrent_per_client: 5,
            ..Default::default()
        };

        let limiter = RateLimiter_::new(config);

        // First request should succeed
        let result = limiter.check_request("client1", "/test.Method");
        assert!(result.is_ok());
    }

    #[test]
    fn test_exempt_methods() {
        let config = RateLimitConfig {
            enabled: true,
            global_rps: 1,
            burst_size: 1,
            per_client_rps: 1,
            per_client_burst: 1,
            max_concurrent_per_client: 1,
            exempt_methods: vec!["/health".to_string()],
            ..Default::default()
        };

        let limiter = RateLimiter_::new(config);

        // Exempt method should always succeed
        for _ in 0..10 {
            let result = limiter.check_request("client1", "/health");
            assert!(result.is_ok());
        }
    }
}

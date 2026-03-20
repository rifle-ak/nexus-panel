//! Request tracing middleware with correlation IDs and distributed tracing.
//!
//! Provides enterprise-grade observability through:
//! - Unique correlation IDs for request tracking
//! - OpenTelemetry integration for distributed tracing
//! - Request/response logging with timing
//! - Baggage propagation for cross-service context
//!
//! # Headers
//!
//! - `X-Request-ID`: Unique request identifier (generated if not provided)
//! - `X-Correlation-ID`: Cross-service correlation ID
//! - `traceparent`: W3C Trace Context propagation
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::tracing_middleware::{TracingConfig, TracingLayer};
//!
//! let config = TracingConfig::from_env();
//! let layer = TracingLayer::new(config);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, info_span, warn, Instrument, Span};
use uuid::Uuid;

/// Tracing configuration
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// Enable request tracing
    pub enabled: bool,
    /// Service name for tracing
    pub service_name: String,
    /// OTLP endpoint for trace export
    pub otlp_endpoint: Option<String>,
    /// Sample rate (0.0 - 1.0)
    pub sample_rate: f64,
    /// Log request headers
    pub log_headers: bool,
    /// Log request body (for debugging)
    pub log_body: bool,
    /// Maximum body size to log
    pub max_body_log_size: usize,
    /// Headers to propagate
    pub propagate_headers: Vec<String>,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            service_name: "nexus-node".to_string(),
            otlp_endpoint: None,
            sample_rate: 1.0,
            log_headers: true,
            log_body: false,
            max_body_log_size: 4096,
            propagate_headers: vec![
                "x-request-id".to_string(),
                "x-correlation-id".to_string(),
                "traceparent".to_string(),
                "tracestate".to_string(),
            ],
        }
    }
}

impl TracingConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("TRACING_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }

        if let Ok(name) = std::env::var("SERVICE_NAME") {
            config.service_name = name;
        }

        if let Ok(endpoint) = std::env::var("OTLP_ENDPOINT") {
            config.otlp_endpoint = Some(endpoint);
        }

        if let Ok(rate) = std::env::var("TRACING_SAMPLE_RATE") {
            config.sample_rate = rate.parse().unwrap_or(1.0);
        }

        if let Ok(log_headers) = std::env::var("TRACING_LOG_HEADERS") {
            config.log_headers = log_headers.to_lowercase() == "true" || log_headers == "1";
        }

        config
    }
}

/// Request context containing tracing information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    /// Unique request ID
    pub request_id: String,
    /// Correlation ID for cross-service tracking
    pub correlation_id: String,
    /// Parent span ID (if part of distributed trace)
    pub parent_span_id: Option<String>,
    /// Trace ID
    pub trace_id: Option<String>,
    /// Start time of request
    #[serde(skip)]
    pub start_time: Option<Instant>,
    /// Request method/path
    pub method: String,
    /// Source IP address
    pub source_ip: Option<String>,
    /// User agent
    pub user_agent: Option<String>,
    /// Actor ID (if authenticated)
    pub actor_id: Option<String>,
    /// Tenant ID (for multi-tenant)
    pub tenant_id: Option<String>,
    /// Custom attributes
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

impl RequestContext {
    /// Create a new request context
    pub fn new(method: impl Into<String>) -> Self {
        let request_id = Uuid::new_v4().to_string();
        Self {
            request_id: request_id.clone(),
            correlation_id: request_id, // Use request ID as correlation ID if not provided
            parent_span_id: None,
            trace_id: None,
            start_time: Some(Instant::now()),
            method: method.into(),
            source_ip: None,
            user_agent: None,
            actor_id: None,
            tenant_id: None,
            attributes: HashMap::new(),
        }
    }

    /// Create from HTTP headers
    pub fn from_headers(headers: &http::HeaderMap, method: &str) -> Self {
        let request_id = headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let correlation_id = headers
            .get("x-correlation-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_else(|| request_id.clone());

        let trace_id = headers.get("traceparent").and_then(|v| v.to_str().ok()).and_then(|tp| {
            // Parse W3C traceparent: version-trace_id-parent_id-flags
            let parts: Vec<&str> = tp.split('-').collect();
            if parts.len() >= 2 {
                Some(parts[1].to_string())
            } else {
                None
            }
        });

        let parent_span_id =
            headers.get("traceparent").and_then(|v| v.to_str().ok()).and_then(|tp| {
                let parts: Vec<&str> = tp.split('-').collect();
                if parts.len() >= 3 {
                    Some(parts[2].to_string())
                } else {
                    None
                }
            });

        let source_ip = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(',').next().unwrap_or("").trim().to_string())
            .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()).map(String::from));

        let user_agent = headers.get("user-agent").and_then(|v| v.to_str().ok()).map(String::from);

        Self {
            request_id,
            correlation_id,
            parent_span_id,
            trace_id,
            start_time: Some(Instant::now()),
            method: method.to_string(),
            source_ip,
            user_agent,
            actor_id: None,
            tenant_id: None,
            attributes: HashMap::new(),
        }
    }

    /// Get elapsed time since request start
    pub fn elapsed(&self) -> Duration {
        self.start_time.map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
    }

    /// Add an attribute
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Set actor ID
    pub fn with_actor(mut self, actor_id: impl Into<String>) -> Self {
        self.actor_id = Some(actor_id.into());
        self
    }

    /// Set tenant ID
    pub fn with_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Create a tracing span for this request
    pub fn span(&self) -> Span {
        info_span!(
            "request",
            request_id = %self.request_id,
            correlation_id = %self.correlation_id,
            method = %self.method,
            source_ip = ?self.source_ip,
            actor_id = ?self.actor_id,
        )
    }

    /// Generate response headers with tracing information
    pub fn response_headers(&self) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("x-request-id".to_string(), self.request_id.clone());
        headers.insert("x-correlation-id".to_string(), self.correlation_id.clone());
        headers
    }
}

/// Request timing information
#[derive(Debug, Clone)]
pub struct RequestTiming {
    pub request_id: String,
    pub method: String,
    pub status_code: u16,
    pub duration_ms: f64,
    pub correlation_id: String,
}

impl RequestTiming {
    pub fn from_context(ctx: &RequestContext, status_code: u16) -> Self {
        Self {
            request_id: ctx.request_id.clone(),
            method: ctx.method.clone(),
            status_code,
            duration_ms: ctx.elapsed().as_secs_f64() * 1000.0,
            correlation_id: ctx.correlation_id.clone(),
        }
    }
}

/// Tower layer for request tracing
pub mod layer {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::body::BoxBody;
    use tower::{Layer, Service};

    /// Tracing layer
    #[derive(Clone)]
    pub struct TracingLayer {
        config: Arc<TracingConfig>,
    }

    impl TracingLayer {
        pub fn new(config: TracingConfig) -> Self {
            Self {
                config: Arc::new(config),
            }
        }
    }

    impl<S> Layer<S> for TracingLayer {
        type Service = TracingService<S>;

        fn layer(&self, service: S) -> Self::Service {
            TracingService {
                inner: service,
                config: self.config.clone(),
            }
        }
    }

    /// Tracing service wrapper
    #[derive(Clone)]
    pub struct TracingService<S> {
        inner: S,
        config: Arc<TracingConfig>,
    }

    impl<S, B> Service<http::Request<B>> for TracingService<S>
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
            let config = self.config.clone();
            let mut inner = self.inner.clone();
            let method = request.uri().path().to_string();

            // Extract tracing context from headers
            let ctx = RequestContext::from_headers(request.headers(), &method);

            // Log request headers if enabled
            if config.log_headers {
                let headers: Vec<_> = request
                    .headers()
                    .iter()
                    .filter(|(name, _)| {
                        // Don't log sensitive headers
                        let name_str = name.as_str().to_lowercase();
                        !name_str.contains("auth")
                            && !name_str.contains("key")
                            && !name_str.contains("secret")
                            && !name_str.contains("token")
                    })
                    .map(|(k, v)| format!("{}={}", k.as_str(), v.to_str().unwrap_or("<binary>")))
                    .collect();

                debug!(
                    request_id = %ctx.request_id,
                    "Request headers: {:?}",
                    headers
                );
            }

            let span = ctx.span();

            Box::pin(
                async move {
                    info!(
                        request_id = %ctx.request_id,
                        correlation_id = %ctx.correlation_id,
                        method = %method,
                        source_ip = ?ctx.source_ip,
                        "Request started"
                    );

                    let result = inner.call(request).await;

                    let status_code = match &result {
                        Ok(resp) => resp.status().as_u16(),
                        Err(_) => 500,
                    };

                    let timing = RequestTiming::from_context(&ctx, status_code);

                    if status_code >= 400 {
                        warn!(
                            request_id = %timing.request_id,
                            correlation_id = %timing.correlation_id,
                            method = %timing.method,
                            status = status_code,
                            duration_ms = timing.duration_ms,
                            "Request failed"
                        );
                    } else {
                        info!(
                            request_id = %timing.request_id,
                            correlation_id = %timing.correlation_id,
                            method = %timing.method,
                            status = status_code,
                            duration_ms = timing.duration_ms,
                            "Request completed"
                        );
                    }

                    // Add response headers
                    if let Ok(mut resp) = result {
                        let headers = resp.headers_mut();
                        if let Ok(request_id) = ctx.request_id.parse() {
                            headers.insert("x-request-id", request_id);
                        }
                        if let Ok(correlation_id) = ctx.correlation_id.parse() {
                            headers.insert("x-correlation-id", correlation_id);
                        }
                        Ok(resp)
                    } else {
                        result
                    }
                }
                .instrument(span),
            )
        }
    }
}

/// Context propagator for outgoing requests
pub struct ContextPropagator;

impl ContextPropagator {
    /// Inject tracing context into outgoing request headers
    pub fn inject(ctx: &RequestContext, headers: &mut http::HeaderMap) {
        if let Ok(value) = ctx.request_id.parse() {
            headers.insert("x-request-id", value);
        }
        if let Ok(value) = ctx.correlation_id.parse() {
            headers.insert("x-correlation-id", value);
        }

        // Generate W3C traceparent if we have trace info
        if let Some(ref trace_id) = ctx.trace_id {
            let span_id = &ctx.request_id[..16.min(ctx.request_id.len())];
            let traceparent = format!("00-{}-{}-01", trace_id, span_id);
            if let Ok(value) = traceparent.parse() {
                headers.insert("traceparent", value);
            }
        }
    }

    /// Extract tracing context from incoming request headers
    pub fn extract(headers: &http::HeaderMap) -> RequestContext {
        RequestContext::from_headers(headers, "unknown")
    }
}

/// Helper macro for creating spans with request context
#[macro_export]
macro_rules! request_span {
    ($ctx:expr, $name:expr) => {
        tracing::info_span!(
            $name,
            request_id = %$ctx.request_id,
            correlation_id = %$ctx.correlation_id,
        )
    };
    ($ctx:expr, $name:expr, $($field:tt)*) => {
        tracing::info_span!(
            $name,
            request_id = %$ctx.request_id,
            correlation_id = %$ctx.correlation_id,
            $($field)*
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_context_new() {
        let ctx = RequestContext::new("/test/method");
        assert!(!ctx.request_id.is_empty());
        assert_eq!(ctx.request_id, ctx.correlation_id);
        assert_eq!(ctx.method, "/test/method");
    }

    #[test]
    fn test_request_context_from_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-request-id", "test-request-123".parse().unwrap());
        headers.insert("x-correlation-id", "test-correlation-456".parse().unwrap());
        headers.insert("x-forwarded-for", "192.168.1.1, 10.0.0.1".parse().unwrap());
        headers.insert("user-agent", "test-agent/1.0".parse().unwrap());

        let ctx = RequestContext::from_headers(&headers, "/api/test");

        assert_eq!(ctx.request_id, "test-request-123");
        assert_eq!(ctx.correlation_id, "test-correlation-456");
        assert_eq!(ctx.source_ip, Some("192.168.1.1".to_string()));
        assert_eq!(ctx.user_agent, Some("test-agent/1.0".to_string()));
    }

    #[test]
    fn test_request_context_generates_ids() {
        let headers = http::HeaderMap::new();
        let ctx = RequestContext::from_headers(&headers, "/api/test");

        // Should generate UUIDs when headers are missing
        assert!(!ctx.request_id.is_empty());
        assert!(!ctx.correlation_id.is_empty());
    }

    #[test]
    fn test_traceparent_parsing() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".parse().unwrap(),
        );

        let ctx = RequestContext::from_headers(&headers, "/api/test");

        assert_eq!(
            ctx.trace_id,
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
        assert_eq!(ctx.parent_span_id, Some("00f067aa0ba902b7".to_string()));
    }

    #[test]
    fn test_response_headers() {
        let ctx = RequestContext::new("/test");
        let headers = ctx.response_headers();

        assert!(headers.contains_key("x-request-id"));
        assert!(headers.contains_key("x-correlation-id"));
    }

    #[test]
    fn test_request_timing() {
        let ctx = RequestContext::new("/test/method");
        std::thread::sleep(std::time::Duration::from_millis(10));

        let timing = RequestTiming::from_context(&ctx, 200);

        assert!(timing.duration_ms >= 10.0);
        assert_eq!(timing.status_code, 200);
        assert_eq!(timing.method, "/test/method");
    }
}

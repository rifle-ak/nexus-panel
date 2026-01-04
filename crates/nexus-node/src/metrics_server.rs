//! HTTP server for Prometheus metrics
//!
//! Exposes Prometheus metrics at /metrics endpoint

use crate::metrics::Metrics;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};

/// Start the Prometheus metrics HTTP server
pub async fn start_metrics_server(
    metrics: Arc<Metrics>,
    bind_addr: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/health", get(health_handler))
        .with_state(metrics);

    let addr = bind_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!("Prometheus metrics server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Handler for /metrics endpoint
async fn metrics_handler(State(metrics): State<Arc<Metrics>>) -> impl IntoResponse {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();

    let metric_families = metrics.registry().gather();
    let mut buffer = Vec::new();

    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        error!("Failed to encode metrics: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            "Failed to encode metrics".to_string(),
        ).into_response();
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        String::from_utf8(buffer).unwrap_or_default(),
    )
        .into_response()
}

/// Handler for /health endpoint (simple health check)
async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}


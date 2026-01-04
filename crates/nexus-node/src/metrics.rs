//! Prometheus metrics for Nexus Node
//!
//! This module provides comprehensive metrics collection for monitoring
//! container operations, resource usage, and system health.

use crate::error::{NodeError, Result};
use prometheus::{
    Counter, CounterVec, Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, Opts, Registry,
};
use std::sync::Arc;
use std::time::Duration;

/// Metrics collection for Nexus Node
#[derive(Clone)]
pub struct Metrics {
    /// Total number of containers managed
    pub containers_total: Gauge,

    /// Number of containers currently running
    pub containers_running: Gauge,

    /// Number of containers in each state
    pub containers_by_state: GaugeVec,

    /// Total number of container operations
    pub container_operations_total: CounterVec,

    /// Duration of container operations
    pub container_operation_duration: HistogramVec,

    /// Total number of gRPC requests
    pub grpc_requests_total: CounterVec,

    /// Duration of gRPC requests
    pub grpc_request_duration: HistogramVec,

    /// Container resource usage (CPU in millicores)
    pub container_cpu_usage: GaugeVec,

    /// Container resource usage (Memory in bytes)
    pub container_memory_usage: GaugeVec,

    /// Container resource usage (Network bytes)
    pub container_network_bytes: CounterVec,

    /// Container resource usage (Disk bytes)
    pub container_disk_bytes: CounterVec,

    /// Container restart count
    pub container_restarts_total: CounterVec,

    /// Image pull duration
    pub image_pull_duration: Histogram,

    /// Image pull errors
    pub image_pull_errors_total: Counter,

    /// Node uptime in seconds
    pub node_uptime: Gauge,

    /// Registry for all metrics
    registry: Registry,
}

impl Metrics {
    /// Create a new metrics instance
    pub fn new() -> Result<Self> {
        let registry = Registry::new();

        // Container metrics
        let containers_total = Gauge::with_opts(Opts::new(
            "nexus_node_containers_total",
            "Total number of containers managed",
        ))?;

        let containers_running = Gauge::with_opts(Opts::new(
            "nexus_node_containers_running",
            "Number of containers currently running",
        ))?;

        let containers_by_state = GaugeVec::new(
            Opts::new(
                "nexus_node_containers_by_state",
                "Number of containers by state",
            ),
            &["state"],
        )?;

        let container_operations_total = CounterVec::new(
            Opts::new(
                "nexus_node_container_operations_total",
                "Total number of container operations",
            ),
            &["operation", "status"],
        )?;

        let container_operation_duration = HistogramVec::new(
            HistogramOpts::new(
                "nexus_node_container_operation_duration_seconds",
                "Duration of container operations in seconds",
            )
            .buckets(vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]),
            &["operation"],
        )?;

        // gRPC metrics
        let grpc_requests_total = CounterVec::new(
            Opts::new(
                "nexus_node_grpc_requests_total",
                "Total number of gRPC requests",
            ),
            &["method", "status"],
        )?;

        let grpc_request_duration = HistogramVec::new(
            HistogramOpts::new(
                "nexus_node_grpc_request_duration_seconds",
                "Duration of gRPC requests in seconds",
            )
            .buckets(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0]),
            &["method"],
        )?;

        // Resource metrics
        let container_cpu_usage = GaugeVec::new(
            Opts::new(
                "nexus_node_container_cpu_usage_millicores",
                "Container CPU usage in millicores",
            ),
            &["container_id", "container_name"],
        )?;

        let container_memory_usage = GaugeVec::new(
            Opts::new(
                "nexus_node_container_memory_usage_bytes",
                "Container memory usage in bytes",
            ),
            &["container_id", "container_name", "type"],
        )?;

        let container_network_bytes = CounterVec::new(
            Opts::new(
                "nexus_node_container_network_bytes_total",
                "Total network bytes transferred",
            ),
            &["container_id", "container_name", "direction"],
        )?;

        let container_disk_bytes = CounterVec::new(
            Opts::new(
                "nexus_node_container_disk_bytes_total",
                "Total disk bytes read/written",
            ),
            &["container_id", "container_name", "operation"],
        )?;

        let container_restarts_total = CounterVec::new(
            Opts::new(
                "nexus_node_container_restarts_total",
                "Total number of container restarts",
            ),
            &["container_id", "container_name"],
        )?;

        // Image metrics
        let image_pull_duration = Histogram::with_opts(
            HistogramOpts::new(
                "nexus_node_image_pull_duration_seconds",
                "Duration of image pulls in seconds",
            )
            .buckets(vec![1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0]),
        )?;

        let image_pull_errors_total = Counter::with_opts(Opts::new(
            "nexus_node_image_pull_errors_total",
            "Total number of image pull errors",
        ))?;

        // Node metrics
        let node_uptime = Gauge::with_opts(Opts::new(
            "nexus_node_uptime_seconds",
            "Node uptime in seconds",
        ))?;

        // Register all metrics
        registry.register(Box::new(containers_total.clone()))?;
        registry.register(Box::new(containers_running.clone()))?;
        registry.register(Box::new(containers_by_state.clone()))?;
        registry.register(Box::new(container_operations_total.clone()))?;
        registry.register(Box::new(container_operation_duration.clone()))?;
        registry.register(Box::new(grpc_requests_total.clone()))?;
        registry.register(Box::new(grpc_request_duration.clone()))?;
        registry.register(Box::new(container_cpu_usage.clone()))?;
        registry.register(Box::new(container_memory_usage.clone()))?;
        registry.register(Box::new(container_network_bytes.clone()))?;
        registry.register(Box::new(container_disk_bytes.clone()))?;
        registry.register(Box::new(container_restarts_total.clone()))?;
        registry.register(Box::new(image_pull_duration.clone()))?;
        registry.register(Box::new(image_pull_errors_total.clone()))?;
        registry.register(Box::new(node_uptime.clone()))?;

        Ok(Self {
            containers_total,
            containers_running,
            containers_by_state,
            container_operations_total,
            container_operation_duration,
            grpc_requests_total,
            grpc_request_duration,
            container_cpu_usage,
            container_memory_usage,
            container_network_bytes,
            container_disk_bytes,
            container_restarts_total,
            image_pull_duration,
            image_pull_errors_total,
            node_uptime,
            registry,
        })
    }

    /// Get the Prometheus registry
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Record a container operation
    pub fn record_container_operation(&self, operation: &str, status: &str, duration: Duration) {
        self.container_operations_total
            .with_label_values(&[operation, status])
            .inc();
        self.container_operation_duration
            .with_label_values(&[operation])
            .observe(duration.as_secs_f64());
    }

    /// Record a gRPC request
    pub fn record_grpc_request(&self, method: &str, status: &str, duration: Duration) {
        self.grpc_requests_total
            .with_label_values(&[method, status])
            .inc();
        self.grpc_request_duration
            .with_label_values(&[method])
            .observe(duration.as_secs_f64());
    }

    /// Update container count metrics
    pub fn update_container_counts(&self, total: usize, running: usize, by_state: &[(&str, usize)]) {
        self.containers_total.set(total as f64);
        self.containers_running.set(running as f64);

        // Reset all state gauges
        for state in &["created", "running", "stopped", "paused", "failed"] {
            self.containers_by_state.with_label_values(&[state]).set(0.0);
        }

        // Set current state counts
        for (state, count) in by_state {
            self.containers_by_state
                .with_label_values(&[state])
                .set(*count as f64);
        }
    }

    /// Update container resource usage
    pub fn update_container_resources(
        &self,
        container_id: &str,
        container_name: &str,
        cpu_millicores: u64,
        memory_bytes: u64,
        memory_type: &str,
    ) {
        self.container_cpu_usage
            .with_label_values(&[container_id, container_name])
            .set(cpu_millicores as f64);

        self.container_memory_usage
            .with_label_values(&[container_id, container_name, memory_type])
            .set(memory_bytes as f64);
    }

    /// Record container restart
    pub fn record_container_restart(&self, container_id: &str, container_name: &str) {
        self.container_restarts_total
            .with_label_values(&[container_id, container_name])
            .inc();
    }

    /// Record image pull
    pub fn record_image_pull(&self, duration: Duration, success: bool) {
        if success {
            self.image_pull_duration.observe(duration.as_secs_f64());
        } else {
            self.image_pull_errors_total.inc();
        }
    }

    /// Update node uptime
    pub fn update_uptime(&self, uptime_secs: u64) {
        self.node_uptime.set(uptime_secs as f64);
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new().expect("Failed to create metrics")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_metrics_creation() {
        let metrics = Metrics::new().unwrap();
        assert_eq!(metrics.containers_total.get(), 0.0);
        assert_eq!(metrics.containers_running.get(), 0.0);
    }

    #[test]
    fn test_container_operation_recording() {
        let metrics = Metrics::new().unwrap();
        metrics.record_container_operation("create", "success", Duration::from_secs(1));
        assert_eq!(
            metrics
                .container_operations_total
                .with_label_values(&["create", "success"])
                .get(),
            1.0
        );
    }

    #[test]
    fn test_grpc_request_recording() {
        let metrics = Metrics::new().unwrap();
        metrics.record_grpc_request("CreateContainer", "ok", Duration::from_millis(100));
        assert_eq!(
            metrics
                .grpc_requests_total
                .with_label_values(&["CreateContainer", "ok"])
                .get(),
            1.0
        );
    }

    #[test]
    fn test_container_counts_update() {
        let metrics = Metrics::new().unwrap();
        metrics.update_container_counts(
            10,
            5,
            &[("running", 5), ("stopped", 3), ("created", 2)],
        );
        assert_eq!(metrics.containers_total.get(), 10.0);
        assert_eq!(metrics.containers_running.get(), 5.0);
        assert_eq!(
            metrics
                .containers_by_state
                .with_label_values(&["running"])
                .get(),
            5.0
        );
    }
}



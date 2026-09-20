//! Resource monitoring: what each server and the node are using, sampled on
//! a schedule and kept as a short history.
//!
//! The runtime hands back cumulative counters (CPU time, bytes written);
//! the monitor turns consecutive readings into rates, keeps the last few
//! minutes for graphs, and feeds the Prometheus gauges. Reading happens
//! here, on one clock, rather than on every page load, so the panel and
//! `/metrics` show the same numbers and a busy dashboard costs nothing
//! extra.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

use crate::container::ContainerManager;
use crate::metrics::Metrics;
use crate::runtime::ContainerStats;

/// How often the monitor samples.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// Samples kept per series: ten minutes at the default interval.
pub const HISTORY_LEN: usize = 120;

/// One server's usage at one moment.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceSample {
    /// Unix time of the sample, in seconds.
    pub at: u64,
    /// CPU use as a percentage of one core (200 = two cores busy).
    pub cpu_percent: f64,
    /// Memory in use, in bytes, including page cache.
    pub memory_bytes: u64,
    /// Anonymous memory alone: what the process holds.
    pub memory_anon_bytes: u64,
    /// The memory limit; `None` when unlimited.
    pub memory_limit_bytes: Option<u64>,
    /// Processes and threads.
    pub pids: u64,
    /// Bytes read from disk since start.
    pub io_read_bytes: u64,
    /// Bytes written to disk since start.
    pub io_write_bytes: u64,
    /// Disk read rate since the previous sample, bytes per second.
    pub io_read_bps: f64,
    /// Disk write rate since the previous sample, bytes per second.
    pub io_write_bps: f64,
}

/// The node's own usage at one moment.
#[derive(Debug, Clone, Serialize)]
pub struct NodeSample {
    pub at: u64,
    /// CPU use across all cores, 0–100.
    pub cpu_percent: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub load_1: f64,
    pub load_5: f64,
    pub load_15: f64,
    pub containers_running: usize,
}

struct ContainerSeries {
    /// The previous raw reading, for rates.
    last: Option<(Instant, ContainerStats)>,
    history: VecDeque<ResourceSample>,
}

/// Samples every running container and the node, on a schedule.
pub struct ResourceMonitor {
    manager: ContainerManager,
    metrics: Arc<Metrics>,
    containers: RwLock<HashMap<String, ContainerSeries>>,
    node: RwLock<VecDeque<NodeSample>>,
    /// sysinfo's CPU numbers are deltas between refreshes, so one `System`
    /// lives for the life of the monitor.
    system: Mutex<sysinfo::System>,
}

impl ResourceMonitor {
    pub fn new(manager: ContainerManager, metrics: Arc<Metrics>) -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_cpu_usage();
        system.refresh_memory();
        Self {
            manager,
            metrics,
            containers: RwLock::new(HashMap::new()),
            node: RwLock::new(VecDeque::with_capacity(HISTORY_LEN)),
            system: Mutex::new(system),
        }
    }

    /// Sample for the life of the process.
    pub fn start(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                this.sample().await;
            }
        });
    }

    /// Take one sample of everything.
    pub async fn sample(&self) {
        let now = Instant::now();
        let at = epoch_secs();
        let containers = self.manager.list_containers().await;
        let running = containers.iter().filter(|c| c.status.is_running()).count();

        let mut series = self.containers.write().await;
        // Forget servers that no longer exist.
        series.retain(|id, _| containers.iter().any(|c| &c.id == id));

        for c in &containers {
            let entry = series.entry(c.id.clone()).or_insert_with(|| ContainerSeries {
                last: None,
                history: VecDeque::with_capacity(HISTORY_LEN),
            });
            if !c.status.is_running() {
                entry.last = None;
                continue;
            }
            let stats = match self.manager.raw_stats(&c.id).await {
                Ok(s) => s,
                Err(e) => {
                    debug!("No stats for {}: {}", c.id, e);
                    entry.last = None;
                    continue;
                }
            };
            let sample = make_sample(at, now, entry.last.as_ref(), &stats);
            entry.last = Some((now, stats));
            push(&mut entry.history, sample.clone());
            self.metrics.update_container_resources(
                &c.id,
                &c.name,
                (sample.cpu_percent * 10.0) as u64,
                sample.memory_anon_bytes,
                "rss",
            );
        }
        drop(series);

        let node_sample = {
            let mut sys = self.system.lock().await;
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            let load = sysinfo::System::load_average();
            NodeSample {
                at,
                cpu_percent: sys.global_cpu_usage() as f64,
                memory_used_bytes: sys.used_memory(),
                memory_total_bytes: sys.total_memory(),
                swap_used_bytes: sys.used_swap(),
                swap_total_bytes: sys.total_swap(),
                load_1: load.one,
                load_5: load.five,
                load_15: load.fifteen,
                containers_running: running,
            }
        };
        push(&mut *self.node.write().await, node_sample);
    }

    /// A server's latest sample, if it is running and has been sampled.
    pub async fn usage(&self, id: &str) -> Option<ResourceSample> {
        let series = self.containers.read().await;
        let s = series.get(id)?;
        s.last.as_ref()?;
        s.history.back().cloned()
    }

    /// Every running server's latest sample.
    pub async fn all_usage(&self) -> HashMap<String, ResourceSample> {
        self.containers
            .read()
            .await
            .iter()
            .filter(|(_, s)| s.last.is_some())
            .filter_map(|(id, s)| s.history.back().cloned().map(|h| (id.clone(), h)))
            .collect()
    }

    /// A server's recent samples, oldest first.
    pub async fn history(&self, id: &str) -> Vec<ResourceSample> {
        self.containers
            .read()
            .await
            .get(id)
            .map(|s| s.history.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The node's latest sample.
    pub async fn node(&self) -> Option<NodeSample> {
        self.node.read().await.back().cloned()
    }

    /// The node's recent samples, oldest first.
    pub async fn node_history(&self) -> Vec<NodeSample> {
        self.node.read().await.iter().cloned().collect()
    }
}

fn push<T>(history: &mut VecDeque<T>, sample: T) {
    if history.len() >= HISTORY_LEN {
        history.pop_front();
    }
    history.push_back(sample);
}

fn epoch_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Turn a raw reading into a sample, with rates against the previous one.
/// The first reading after a start has no rates: CPU shows as zero rather
/// than as the whole of its accumulated time squeezed into one interval.
pub fn make_sample(
    at: u64,
    now: Instant,
    last: Option<&(Instant, ContainerStats)>,
    stats: &ContainerStats,
) -> ResourceSample {
    let (cpu_percent, io_read_bps, io_write_bps) = match last {
        Some((then, prev)) => {
            let wall = now.duration_since(*then).as_secs_f64();
            if wall <= 0.0 {
                (0.0, 0.0, 0.0)
            } else {
                let cpu =
                    stats.cpu_usage_usec.saturating_sub(prev.cpu_usage_usec) as f64 / 1_000_000.0;
                (
                    round1(cpu / wall * 100.0),
                    round1(stats.io_read_bytes.saturating_sub(prev.io_read_bytes) as f64 / wall),
                    round1(stats.io_write_bytes.saturating_sub(prev.io_write_bytes) as f64 / wall),
                )
            }
        }
        None => (0.0, 0.0, 0.0),
    };
    ResourceSample {
        at,
        cpu_percent,
        memory_bytes: stats.memory_bytes,
        memory_anon_bytes: stats.memory_anon_bytes,
        memory_limit_bytes: stats.memory_limit_bytes,
        pids: stats.pids,
        io_read_bytes: stats.io_read_bytes,
        io_write_bytes: stats.io_write_bytes,
        io_read_bps,
        io_write_bps,
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_come_from_consecutive_readings() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(2);
        let prev = ContainerStats {
            cpu_usage_usec: 1_000_000,
            io_read_bytes: 1000,
            io_write_bytes: 0,
            ..Default::default()
        };
        let cur = ContainerStats {
            cpu_usage_usec: 4_000_000, // 3 s of CPU in 2 s of wall: 150%
            memory_bytes: 10,
            memory_anon_bytes: 8,
            memory_limit_bytes: Some(100),
            pids: 3,
            io_read_bytes: 3000,
            io_write_bytes: 500,
        };
        let s = make_sample(7, t1, Some(&(t0, prev)), &cur);
        assert_eq!(s.cpu_percent, 150.0);
        assert_eq!(s.io_read_bps, 1000.0);
        assert_eq!(s.io_write_bps, 250.0);
        assert_eq!(s.memory_bytes, 10);
        assert_eq!(s.memory_limit_bytes, Some(100));
        assert_eq!(s.at, 7);

        let first = make_sample(7, t1, None, &cur);
        assert_eq!(first.cpu_percent, 0.0);
        assert_eq!(first.pids, 3);
    }

    #[test]
    fn history_is_bounded() {
        let mut h = VecDeque::new();
        for i in 0..(HISTORY_LEN + 5) {
            push(&mut h, i);
        }
        assert_eq!(h.len(), HISTORY_LEN);
        assert_eq!(*h.front().unwrap(), 5);
    }

    #[tokio::test]
    async fn samples_running_containers_from_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let manager = ContainerManager::new(dir.path().to_path_buf());
        let metrics = Arc::new(Metrics::new().unwrap());
        let monitor = ResourceMonitor::new(manager.clone(), metrics);

        monitor.sample().await;
        assert!(monitor.node().await.is_some());
        assert!(monitor.all_usage().await.is_empty());

        // A blueprint with no install step, so the server starts at once.
        let yaml = r#"
metadata: { id: simple, name: Simple, version: "1", game: simple, author: test }
container: { image: example/simple:latest }
resources:
  cpu: { min: 500, max: 1000, shares: 1024 }
  memory: { min: 512Mi, max: 1Gi }
  disk: { min: 1Gi }
startup: { command: /bin/true, working_dir: /home/container }
variables: []
networking: { ports: [] }
security: { capabilities: { drop: [], add: [] } }
"#;
        let config: nexus_config::GameConfig = serde_yaml::from_str(yaml).unwrap();
        let id = manager.create_container(&config, None).await.unwrap();
        manager.start_container(&id).await.unwrap();

        monitor.sample().await;
        monitor.sample().await;
        let usage = monitor.usage(&id).await.expect("sampled");
        assert!(usage.memory_bytes > 0);
        assert!(usage.memory_limit_bytes.is_some());
        assert_eq!(monitor.history(&id).await.len(), 2);
        assert_eq!(monitor.all_usage().await.len(), 1);

        manager.stop_container(&id, Some(1)).await.unwrap();
        monitor.sample().await;
        assert!(monitor.usage(&id).await.is_none());
        assert_eq!(monitor.history(&id).await.len(), 2);

        manager.delete_container(&id, true).await.unwrap();
        monitor.sample().await;
        assert!(monitor.history(&id).await.is_empty());
    }
}

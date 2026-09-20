//! Reading a container's resource usage from its cgroup.
//!
//! containerd puts each task in its own cgroup; where exactly depends on the
//! runtime's defaults and whether the host runs cgroup v2 (the unified
//! hierarchy, every current distribution) or v1. Rather than guess the path,
//! this looks it up from the task's pid in `/proc/<pid>/cgroup`, then reads
//! the accounting files the kernel keeps there. Everything is parameterised
//! on the `/proc` and `/sys/fs/cgroup` roots so tests run against a fake
//! tree.

use std::path::{Path, PathBuf};

use crate::error::{NodeError, Result};
use crate::runtime::ContainerStats;

/// Where a process's cgroup lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CgroupRef {
    /// Unified hierarchy: one path under the cgroup root.
    V2(PathBuf),
    /// Legacy hierarchy: a path per controller.
    V1 {
        memory: Option<PathBuf>,
        cpu: Option<PathBuf>,
        pids: Option<PathBuf>,
        blkio: Option<PathBuf>,
    },
}

/// Read resource usage for the process `pid` (a container's init).
pub fn stats_for_pid(pid: u32) -> Result<ContainerStats> {
    let proc_root = Path::new("/proc");
    let cgroup_root = Path::new("/sys/fs/cgroup");
    let cg = cgroup_of_pid(proc_root, pid)?;
    read_stats(cgroup_root, &cg)
}

/// Find the cgroup the process belongs to from `<proc_root>/<pid>/cgroup`.
pub fn cgroup_of_pid(proc_root: &Path, pid: u32) -> Result<CgroupRef> {
    let path = proc_root.join(pid.to_string()).join("cgroup");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| NodeError::Internal(format!("cannot read {}: {}", path.display(), e)))?;
    parse_proc_cgroup(&text)
}

/// Parse the contents of `/proc/<pid>/cgroup`.
///
/// Each line is `hierarchy-id:controllers:path`. On v2 there is one line,
/// `0::/path`. On v1 there is a line per mounted controller group.
pub fn parse_proc_cgroup(text: &str) -> Result<CgroupRef> {
    let mut v2 = None;
    let mut memory = None;
    let mut cpu = None;
    let mut pids = None;
    let mut blkio = None;

    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let rel = PathBuf::from(path.trim_start_matches('/'));
        if id == "0" && controllers.is_empty() {
            v2 = Some(rel);
            continue;
        }
        for c in controllers.split(',') {
            match c {
                "memory" => memory = Some(Path::new("memory").join(&rel)),
                "cpu" | "cpuacct" => cpu = Some(Path::new("cpu,cpuacct").join(&rel)),
                "pids" => pids = Some(Path::new("pids").join(&rel)),
                "blkio" => blkio = Some(Path::new("blkio").join(&rel)),
                _ => {}
            }
        }
    }

    if let Some(path) = v2 {
        // A v2 line is present on hybrid hosts too; prefer it only when no
        // v1 controllers are mounted, since then it carries the accounting.
        if memory.is_none() && cpu.is_none() {
            return Ok(CgroupRef::V2(path));
        }
    }
    if memory.is_some() || cpu.is_some() {
        return Ok(CgroupRef::V1 {
            memory,
            cpu,
            pids,
            blkio,
        });
    }
    Err(NodeError::Internal(
        "process has no cgroup with resource accounting".to_string(),
    ))
}

/// Read the accounting files for a cgroup under `root`.
pub fn read_stats(root: &Path, cg: &CgroupRef) -> Result<ContainerStats> {
    match cg {
        CgroupRef::V2(path) => read_v2(&root.join(path)),
        CgroupRef::V1 {
            memory,
            cpu,
            pids,
            blkio,
        } => Ok(read_v1(
            root,
            memory.as_deref(),
            cpu.as_deref(),
            pids.as_deref(),
            blkio.as_deref(),
        )),
    }
}

fn read_v2(dir: &Path) -> Result<ContainerStats> {
    if !dir.is_dir() {
        return Err(NodeError::Internal(format!(
            "cgroup {} does not exist",
            dir.display()
        )));
    }
    let cpu_stat = read_or_empty(&dir.join("cpu.stat"));
    let memory_stat = read_or_empty(&dir.join("memory.stat"));
    let io_stat = read_or_empty(&dir.join("io.stat"));
    let (io_read_bytes, io_write_bytes) = parse_io_stat(&io_stat);
    Ok(ContainerStats {
        cpu_usage_usec: kv_field(&cpu_stat, "usage_usec").unwrap_or(0),
        memory_bytes: read_u64(&dir.join("memory.current")).unwrap_or(0),
        memory_anon_bytes: kv_field(&memory_stat, "anon").unwrap_or(0),
        memory_limit_bytes: read_limit(&dir.join("memory.max")),
        pids: read_u64(&dir.join("pids.current")).unwrap_or(0),
        io_read_bytes,
        io_write_bytes,
    })
}

fn read_v1(
    root: &Path,
    memory: Option<&Path>,
    cpu: Option<&Path>,
    pids: Option<&Path>,
    blkio: Option<&Path>,
) -> ContainerStats {
    let mut stats = ContainerStats::default();
    if let Some(cpu) = cpu {
        // cpuacct.usage is nanoseconds.
        stats.cpu_usage_usec = read_u64(&root.join(cpu).join("cpuacct.usage")).unwrap_or(0) / 1000;
    }
    if let Some(mem) = memory {
        let dir = root.join(mem);
        stats.memory_bytes = read_u64(&dir.join("memory.usage_in_bytes")).unwrap_or(0);
        let memory_stat = read_or_empty(&dir.join("memory.stat"));
        stats.memory_anon_bytes = kv_field(&memory_stat, "total_rss")
            .or_else(|| kv_field(&memory_stat, "rss"))
            .unwrap_or(0);
        stats.memory_limit_bytes = read_limit(&dir.join("memory.limit_in_bytes"));
    }
    if let Some(pids) = pids {
        stats.pids = read_u64(&root.join(pids).join("pids.current")).unwrap_or(0);
    }
    if let Some(blkio) = blkio {
        let text = read_or_empty(&root.join(blkio).join("blkio.throttle.io_service_bytes"));
        let (r, w) = parse_blkio_service_bytes(&text);
        stats.io_read_bytes = r;
        stats.io_write_bytes = w;
    }
    stats
}

fn read_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// A limit file holds a number, or `max` (v2) / a huge sentinel (v1) for
/// unlimited.
fn read_limit(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text == "max" {
        return None;
    }
    let value: u64 = text.parse().ok()?;
    // v1 reports "no limit" as the largest page-aligned value.
    if value >= i64::MAX as u64 / 2 {
        return None;
    }
    Some(value)
}

/// `key value` lines, as in `cpu.stat` and `memory.stat`.
fn kv_field(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        (it.next()? == key).then(|| it.next()?.parse().ok())?
    })
}

/// `io.stat` lines look like `8:0 rbytes=1234 wbytes=5678 rios=1 …`, one
/// per device; totals are the sum.
fn parse_io_stat(text: &str) -> (u64, u64) {
    let mut read = 0;
    let mut write = 0;
    for line in text.lines() {
        for field in line.split_whitespace().skip(1) {
            if let Some(v) = field.strip_prefix("rbytes=") {
                read += v.parse::<u64>().unwrap_or(0);
            } else if let Some(v) = field.strip_prefix("wbytes=") {
                write += v.parse::<u64>().unwrap_or(0);
            }
        }
    }
    (read, write)
}

/// v1 `blkio.throttle.io_service_bytes`: `8:0 Read 1234` / `8:0 Write 5678`.
fn parse_blkio_service_bytes(text: &str) -> (u64, u64) {
    let mut read = 0;
    let mut write = 0;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(_dev), Some(op), Some(v)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let v: u64 = v.parse().unwrap_or(0);
        match op {
            "Read" => read += v,
            "Write" => write += v,
            _ => {}
        }
    }
    (read, write)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn parses_v2_proc_cgroup() {
        assert_eq!(
            parse_proc_cgroup("0::/nexus/abc\n").unwrap(),
            CgroupRef::V2(PathBuf::from("nexus/abc"))
        );
    }

    #[test]
    fn parses_v1_proc_cgroup() {
        let text = "12:pids:/nexus/abc\n5:memory:/nexus/abc\n3:cpu,cpuacct:/nexus/abc\n2:blkio:/nexus/abc\n1:name=systemd:/x\n";
        match parse_proc_cgroup(text).unwrap() {
            CgroupRef::V1 {
                memory,
                cpu,
                pids,
                blkio,
            } => {
                assert_eq!(memory.unwrap(), PathBuf::from("memory/nexus/abc"));
                assert_eq!(cpu.unwrap(), PathBuf::from("cpu,cpuacct/nexus/abc"));
                assert_eq!(pids.unwrap(), PathBuf::from("pids/nexus/abc"));
                assert_eq!(blkio.unwrap(), PathBuf::from("blkio/nexus/abc"));
            }
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn hybrid_host_prefers_v1_controllers() {
        let text = "5:memory:/a\n0::/init.scope\n";
        assert!(matches!(
            parse_proc_cgroup(text).unwrap(),
            CgroupRef::V1 { .. }
        ));
        assert!(parse_proc_cgroup("1:name=systemd:/x\n").is_err());
    }

    #[test]
    fn reads_a_v2_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "proc/4242/cgroup", "0::/nexus/srv1\n");
        write(
            root,
            "cg/nexus/srv1/cpu.stat",
            "usage_usec 1500000\nuser_usec 1000000\nsystem_usec 500000\n",
        );
        write(root, "cg/nexus/srv1/memory.current", "734003200\n");
        write(root, "cg/nexus/srv1/memory.max", "2147483648\n");
        write(
            root,
            "cg/nexus/srv1/memory.stat",
            "anon 629145600\nfile 104857600\n",
        );
        write(root, "cg/nexus/srv1/pids.current", "37\n");
        write(
            root,
            "cg/nexus/srv1/io.stat",
            "8:0 rbytes=1000 wbytes=2000 rios=1 wios=2\n259:0 rbytes=10 wbytes=20 rios=0 wios=0\n",
        );

        let cg = cgroup_of_pid(&root.join("proc"), 4242).unwrap();
        let stats = read_stats(&root.join("cg"), &cg).unwrap();
        assert_eq!(
            stats,
            ContainerStats {
                cpu_usage_usec: 1_500_000,
                memory_bytes: 734_003_200,
                memory_anon_bytes: 629_145_600,
                memory_limit_bytes: Some(2_147_483_648),
                pids: 37,
                io_read_bytes: 1010,
                io_write_bytes: 2020,
            }
        );
    }

    #[test]
    fn unlimited_memory_is_none_and_missing_cgroup_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "cg/x/memory.max", "max\n");
        write(root, "cg/x/cpu.stat", "");
        let stats = read_stats(&root.join("cg"), &CgroupRef::V2("x".into())).unwrap();
        assert_eq!(stats.memory_limit_bytes, None);
        assert_eq!(stats.cpu_usage_usec, 0);
        assert!(read_stats(&root.join("cg"), &CgroupRef::V2("gone".into())).is_err());
        assert!(cgroup_of_pid(&root.join("proc"), 1).is_err());
    }

    /// Against the real kernel: this process's own cgroup. Skipped where
    /// the cgroup filesystem is not visible (some container sandboxes).
    #[test]
    fn reads_own_cgroup_when_available() {
        let root = Path::new("/sys/fs/cgroup");
        let Ok(cg) = cgroup_of_pid(Path::new("/proc"), std::process::id()) else {
            eprintln!("no cgroup for this process; skipping");
            return;
        };
        let dir = match &cg {
            CgroupRef::V2(p) => root.join(p),
            CgroupRef::V1 { cpu, .. } => match cpu {
                Some(p) => root.join(p),
                None => return,
            },
        };
        if !dir.is_dir() {
            eprintln!("{} not visible; skipping", dir.display());
            return;
        }
        let stats = read_stats(root, &cg).expect("own cgroup is readable");
        assert!(stats.cpu_usage_usec > 0, "{:?}", stats);
    }

    #[test]
    fn reads_a_v1_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "cg/cpu,cpuacct/a/cpuacct.usage", "2500000000\n");
        write(root, "cg/memory/a/memory.usage_in_bytes", "100\n");
        write(root, "cg/memory/a/memory.stat", "rss 60\ntotal_rss 70\n");
        write(
            root,
            "cg/memory/a/memory.limit_in_bytes",
            "9223372036854771712\n",
        );
        write(root, "cg/pids/a/pids.current", "3\n");
        write(
            root,
            "cg/blkio/a/blkio.throttle.io_service_bytes",
            "8:0 Read 5\n8:0 Write 6\n8:0 Sync 11\nTotal 11\n",
        );
        let cg = CgroupRef::V1 {
            memory: Some("memory/a".into()),
            cpu: Some("cpu,cpuacct/a".into()),
            pids: Some("pids/a".into()),
            blkio: Some("blkio/a".into()),
        };
        let stats = read_stats(&root.join("cg"), &cg).unwrap();
        assert_eq!(stats.cpu_usage_usec, 2_500_000);
        assert_eq!(stats.memory_bytes, 100);
        assert_eq!(stats.memory_anon_bytes, 70);
        assert_eq!(stats.memory_limit_bytes, None);
        assert_eq!(stats.pids, 3);
        assert_eq!((stats.io_read_bytes, stats.io_write_bytes), (5, 6));
    }
}

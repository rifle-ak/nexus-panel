//! Integration test that exercises the real containerd runtime.
//!
//! This is gated behind `NEXUS_IT_CONTAINERD=1` and a running containerd, so it
//! is a no-op skip in normal `cargo test` runs (developer machines, the default
//! CI test job) and only executes in the dedicated CI integration job that
//! installs and starts containerd. That keeps the default test suite fast and
//! dependency-free while still validating the containerd connection path.

use nexus_node::runtime::{ContainerSpec, ResourceLimits};
use nexus_node::{ContainerRuntime, ContainerdRuntime};
use std::collections::HashMap;

/// Whether the containerd integration test should run.
fn enabled() -> bool {
    std::env::var("NEXUS_IT_CONTAINERD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Connect to a live containerd and verify a round-trip RPC behaves.
///
/// Inspecting a container that does not exist must return a clean error rather
/// than hanging or panicking — this proves the socket connection, the tonic
/// client, and our error mapping all work end-to-end against real containerd.
#[tokio::test]
async fn containerd_connect_and_inspect_missing() {
    if !enabled() {
        eprintln!(
            "skipping containerd integration test \
             (set NEXUS_IT_CONTAINERD=1 with a running containerd to run it)"
        );
        return;
    }

    let socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());

    let runtime = ContainerdRuntime::new(socket, "nexus-integration-test".to_string());

    runtime.connect().await.expect("should connect to the running containerd");

    let result = runtime.inspect("nexus-it-does-not-exist").await;
    assert!(
        result.is_err(),
        "inspecting a non-existent container should return an error, got: {:?}",
        result.map(|i| i.status)
    );
}

/// Build a minimal spec for a container that just prints and exits.
fn spec(image: &str, args: &[&str]) -> ContainerSpec {
    ContainerSpec {
        image: image.to_string(),
        command: vec![],
        args: args.iter().map(|a| a.to_string()).collect(),
        env: HashMap::new(),
        working_dir: "/".to_string(),
        mounts: vec![],
        ports: vec![],
        resources: ResourceLimits {
            cpu_shares: 1024,
            memory_bytes: 256 * 1024 * 1024,
            memory_swap_bytes: 0,
        },
    }
}

/// Pull an image, run a container from it, and read back what it printed.
///
/// This is the path a panel user takes when they pick a blueprint and press
/// start, and it is the one that used to fail twice over: the runtime never
/// pulled anything, and a created container had no root filesystem for its
/// task to run in.
#[tokio::test]
async fn pull_create_start_and_stop_a_container() {
    if !enabled() {
        eprintln!(
            "skipping containerd integration test \
             (set NEXUS_IT_CONTAINERD=1 with a running containerd to run it)"
        );
        return;
    }

    let socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());
    let image = std::env::var("NEXUS_IT_IMAGE")
        .unwrap_or_else(|_| "docker.io/library/alpine:latest".to_string());

    let state_dir = std::env::temp_dir().join(format!("nexus-it-{}", std::process::id()));
    let runtime = ContainerdRuntime::new(socket, "nexus-integration-test".to_string())
        .with_state_dir(state_dir.clone());

    runtime.connect().await.expect("should connect to the running containerd");

    let id = format!("nexus-it-{}", uuid::Uuid::new_v4());

    // Pull has to work on its own: the image is very likely absent from this
    // namespace, and the runtime is expected to fetch it rather than tell the
    // operator to go and run `ctr` by hand.
    runtime.pull_image(&image).await.expect("pull_image should fetch the image");
    // Pulling again is a no-op, not an error.
    runtime.pull_image(&image).await.expect("pull_image should be idempotent");

    runtime
        .create(
            &id,
            spec(&image, &["/bin/sh", "-c", "echo nexus-was-here; sleep 30"]),
        )
        .await
        .expect("create should prepare a rootfs and register the container");

    let pid = runtime.start(&id).await.expect("start should run the container");
    assert!(pid > 0, "a started task should report a pid");

    let info = runtime.inspect(&id).await.expect("inspect should find the container");
    assert_eq!(info.status, "running");

    // The container's own output should reach the console log.
    let mut console = runtime.attach(&id).await.expect("attach should open the console");
    let mut output = String::new();
    for _ in 0..50 {
        while let Ok(Some(line)) = console.read_line().await {
            output.push_str(&line);
        }
        if output.contains("nexus-was-here") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        output.contains("nexus-was-here"),
        "container output should reach the console log, got: {:?}",
        output
    );

    // `exec` runs a second process in the same rootfs.
    let exec = runtime
        .exec(
            &id,
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo from-exec".to_string(),
            ],
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("exec should run inside the container");
    assert_eq!(exec.exit_code, Some(0));
    assert!(
        exec.stdout.contains("from-exec"),
        "exec should capture stdout, got: {:?}",
        exec.stdout
    );

    runtime.stop(&id, 10).await.expect("stop should terminate the task");

    let info = runtime.inspect(&id).await.expect("inspect should still find the container");
    assert_ne!(
        info.status, "running",
        "a stopped container must not report as running"
    );

    runtime.delete(&id).await.expect("delete should remove the container");
    assert!(
        runtime.inspect(&id).await.is_err(),
        "a deleted container should no longer be inspectable"
    );

    let _ = std::fs::remove_dir_all(&state_dir);
}

/// A container can be started again after being stopped.
///
/// Restart reuses the rootfs snapshot created at container-create time; if
/// `delete`ing the task left anything behind, the second start fails.
#[tokio::test]
async fn a_stopped_container_can_be_started_again() {
    if !enabled() {
        eprintln!("skipping containerd integration test (NEXUS_IT_CONTAINERD is not set)");
        return;
    }

    let socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());
    let image = std::env::var("NEXUS_IT_IMAGE")
        .unwrap_or_else(|_| "docker.io/library/alpine:latest".to_string());

    let state_dir = std::env::temp_dir().join(format!("nexus-it-restart-{}", std::process::id()));
    let runtime = ContainerdRuntime::new(socket, "nexus-integration-test".to_string())
        .with_state_dir(state_dir.clone());
    runtime.connect().await.expect("should connect to the running containerd");

    let id = format!("nexus-it-{}", uuid::Uuid::new_v4());
    runtime.pull_image(&image).await.expect("pull_image should fetch the image");
    runtime
        .create(&id, spec(&image, &["/bin/sh", "-c", "sleep 30"]))
        .await
        .expect("create should succeed");

    runtime.start(&id).await.expect("first start should succeed");
    runtime.stop(&id, 10).await.expect("stop should succeed");
    runtime.start(&id).await.expect("a stopped container should start again");
    runtime.stop(&id, 10).await.expect("second stop should succeed");
    runtime.delete(&id).await.expect("delete should succeed");

    let _ = std::fs::remove_dir_all(&state_dir);
}

/// Pulling an image that does not exist must fail with the registry's reason,
/// not with a generic "not found" that leaves the operator guessing.
#[tokio::test]
async fn pulling_a_missing_image_reports_why() {
    if !enabled() {
        eprintln!("skipping containerd integration test (NEXUS_IT_CONTAINERD is not set)");
        return;
    }

    let socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());
    let runtime = ContainerdRuntime::new(socket, "nexus-integration-test".to_string());
    runtime.connect().await.expect("should connect to the running containerd");

    let err = runtime
        .pull_image("docker.io/library/nexus-panel-does-not-exist:nope")
        .await
        .expect_err("pulling a non-existent image should fail");

    let message = err.to_string();
    assert!(
        message.contains("nexus-panel-does-not-exist"),
        "the error should name the image, got: {}",
        message
    );
}

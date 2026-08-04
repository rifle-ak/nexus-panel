//! Integration test that exercises the real containerd runtime.
//!
//! This is gated behind `NEXUS_IT_CONTAINERD=1` and a running containerd, so it
//! is a no-op skip in normal `cargo test` runs (developer machines, the default
//! CI test job) and only executes in the dedicated CI integration job that
//! installs and starts containerd. That keeps the default test suite fast and
//! dependency-free while still validating the containerd connection path.

use nexus_node::{ContainerRuntime, ContainerdRuntime};

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

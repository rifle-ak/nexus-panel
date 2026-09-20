//! Integration test that exercises the real containerd runtime.
//!
//! This is gated behind `NEXUS_IT_CONTAINERD=1` and a running containerd, so it
//! is a no-op skip in normal `cargo test` runs (developer machines, the default
//! CI test job) and only executes in the dedicated CI integration job that
//! installs and starts containerd. That keeps the default test suite fast and
//! dependency-free while still validating the containerd connection path.

use nexus_node::runtime::{ContainerSpec, ResourceLimits, SecurityOptions};
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
            cpu_millicores: Some(1000),
            memory_bytes: 256 * 1024 * 1024,
            memory_swap_bytes: 0,
            pids_limit: Some(256),
            rlimits: Vec::new(),
            nofile: 65536,
        },
        // The test images are tiny (busybox) and run as root; what is
        // exercised here is the runtime path, not file ownership.
        security: SecurityOptions {
            uid: 0,
            gid: 0,
            ..SecurityOptions::default()
        },
        hostname: "nx-test".to_string(),
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

/// A blueprint whose install writes real files into the server directory.
///
/// Deliberately not a SteamCMD install: this test is about the install
/// *mechanism* — a one-shot container, the server directory mounted into it,
/// output captured, exit code honoured — and pinning it to Valve's CDN would
/// make it a network test instead.
fn install_blueprint(image: &str, script: &str) -> String {
    format!(
        r#"
blueprint_version: "1.0"
metadata:
  id: install-it
  name: Install Integration Test
  version: "1.0"
  game: test
  author: test
  description: exercises the install runner
container:
  image: {image}
  pull_policy: if_not_present
resources:
  cpu: {{ min: 100, max: 1000, shares: 512 }}
  memory: {{ min: 128Mi, max: 512Mi }}
  disk: {{ min: 1Gi }}
startup:
  command: /bin/sh
  args: ["-c", "sleep 30"]
  working_dir: /home/container
install:
  script: |
{script}
  timeout: 300s
variables:
  - name: GAME_FILE
    description: file the install creates
    default: server.bin
    required: false
    user_editable: false
    user_viewable: true
networking:
  ports: []
security:
  capabilities: {{ drop: [ALL] }}
  read_only_root: false
  no_new_privileges: true
"#,
        image = image,
        script = script.lines().map(|l| format!("    {}", l)).collect::<Vec<_>>().join("\n"),
    )
}

/// Install a server's game files, then start it.
///
/// This is the whole point of the install step: a blueprint's image carries
/// tooling, not the game, so something has to populate the server directory
/// before the startup command can possibly exist.
#[tokio::test]
async fn install_populates_the_server_directory_and_unblocks_start() {
    if !enabled() {
        eprintln!("skipping containerd integration test (NEXUS_IT_CONTAINERD is not set)");
        return;
    }

    let socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());
    let image = std::env::var("NEXUS_IT_IMAGE")
        .unwrap_or_else(|_| "docker.io/library/alpine:latest".to_string());

    let state_dir = std::env::temp_dir().join(format!("nexus-it-install-{}", std::process::id()));
    let data_dir = state_dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let runtime = std::sync::Arc::new(
        ContainerdRuntime::new(socket, "nexus-integration-test".to_string())
            .with_state_dir(state_dir.clone()),
    );
    runtime.connect().await.expect("should connect to containerd");

    let manager = nexus_node::ContainerManager::with_runtime(runtime.clone(), data_dir.clone());

    // The install writes a file named by a blueprint variable, so this also
    // proves `{{VAR}}` substitution reaches the script.
    let yaml = install_blueprint(
        &image,
        "echo 'installing'\nprintf 'game-files' > {{GAME_FILE}}\nmkdir -p data\n",
    );
    let config = nexus_config::GameConfig::from_yaml(&yaml).expect("blueprint should parse");

    let id = manager.create_container(&config, None).await.expect("create should succeed");

    // A server with an install declared must refuse to start until it runs.
    let blocked = manager.start_container(&id).await;
    assert!(
        blocked.is_err(),
        "a server with no game files should not start"
    );

    // Nothing has populated the directory yet — this is the empty file
    // manager an operator sees before installing.
    assert_eq!(std::fs::read_dir(data_dir.join(&id)).unwrap().count(), 0);

    let mut log = String::new();
    let exit = manager
        .run_install(&id, |line| log.push_str(&line))
        .await
        .expect("install should run");
    assert_eq!(exit, 0, "install failed, log:\n{}", log);
    assert!(
        log.contains("installing"),
        "install output should be captured, got: {:?}",
        log
    );

    // The game files are really there, under the name the variable gave.
    let installed = data_dir.join(&id).join("server.bin");
    assert_eq!(
        std::fs::read_to_string(&installed).expect("install should have written the file"),
        "game-files"
    );

    // And the server can now start.
    assert_eq!(
        manager.get_state(&id).await.unwrap().install_state,
        nexus_node::install::InstallState::Installed
    );
    manager.start_container(&id).await.expect("an installed server should start");

    // The install container must not linger.
    assert!(
        runtime.inspect(&format!("{}-install", id)).await.is_err(),
        "the install container should be cleaned up"
    );

    manager.stop_container(&id, Some(5)).await.expect("stop");

    // Reinstalling is how an operator repairs a server, and it must report
    // only this run: the install container's id is reused, so a stale console
    // log would replay the previous install's output as if it were new.
    let mut second = String::new();
    let exit = manager
        .run_install(&id, |line| second.push_str(&line))
        .await
        .expect("reinstall should run");
    assert_eq!(exit, 0, "reinstall failed, log:\n{}", second);
    assert_eq!(
        second.matches("installing").count(),
        1,
        "reinstall log replayed an earlier run: {:?}",
        second
    );

    manager.delete_container(&id, true).await.expect("delete");
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// A failing install must be reported as failed, and must not let the server
/// start — a half-installed game is worse than an obviously missing one.
#[tokio::test]
async fn a_failing_install_leaves_the_server_blocked() {
    if !enabled() {
        eprintln!("skipping containerd integration test (NEXUS_IT_CONTAINERD is not set)");
        return;
    }

    let socket = std::env::var("CONTAINERD_SOCKET")
        .unwrap_or_else(|_| "/run/containerd/containerd.sock".to_string());
    let image = std::env::var("NEXUS_IT_IMAGE")
        .unwrap_or_else(|_| "docker.io/library/alpine:latest".to_string());

    let state_dir = std::env::temp_dir().join(format!("nexus-it-badinst-{}", std::process::id()));
    let data_dir = state_dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let runtime = std::sync::Arc::new(
        ContainerdRuntime::new(socket, "nexus-integration-test".to_string())
            .with_state_dir(state_dir.clone()),
    );
    runtime.connect().await.expect("should connect to containerd");
    let manager = nexus_node::ContainerManager::with_runtime(runtime.clone(), data_dir.clone());

    // The first step fails; `set -e` must stop the script there.
    let yaml = install_blueprint(
        &image,
        "echo 'about to fail'\nfalse\nprintf 'should-not-exist' > late.bin\n",
    );
    let config = nexus_config::GameConfig::from_yaml(&yaml).unwrap();
    let id = manager.create_container(&config, None).await.expect("create");

    let mut log = String::new();
    let exit = manager.run_install(&id, |line| log.push_str(&line)).await.expect("run");
    assert_ne!(exit, 0, "a failing step must fail the install");
    assert!(log.contains("about to fail"), "log: {:?}", log);

    // `set -e` stopped the script, so the later step never ran.
    assert!(
        !data_dir.join(&id).join("late.bin").exists(),
        "the install continued past a failed step"
    );

    assert_eq!(
        manager.get_state(&id).await.unwrap().install_state,
        nexus_node::install::InstallState::Failed
    );
    assert!(
        manager.start_container(&id).await.is_err(),
        "a server whose install failed must not start"
    );

    manager.delete_container(&id, true).await.expect("delete");
    let _ = std::fs::remove_dir_all(&state_dir);
}

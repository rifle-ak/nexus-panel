//! The installer's configuration merge, exercised against a real shell.
//!
//! `merge_config` runs on every future upgrade of every existing node, and it
//! edits the one file holding an operator's credentials and bind addresses.
//! Getting it wrong is quiet: values are clobbered, or new settings never
//! appear and the node silently uses built-in defaults. So it is tested the
//! only way that proves anything — by running it.

use std::process::Command;

/// Source `install.sh` without its trailing `main "$@"`, then run `body`.
fn run_installer_fn(config_dir: &std::path::Path, body: &str) -> String {
    let installer = concat!(env!("CARGO_MANIFEST_DIR"), "/../../install.sh");
    let source = std::fs::read_to_string(installer).expect("install.sh should be readable");

    // Cut the script at its entry point so sourcing it defines the functions
    // without running the installer. Getting this wrong runs a real install as
    // root, so the marker must be found rather than assumed.
    const ENTRY_POINT: &str = "\nmain \"$@\"";
    let cut = source
        .rfind(ENTRY_POINT)
        .expect("install.sh no longer ends with `main \"$@\"`; this harness needs updating");
    let definitions = &source[..cut];
    assert!(
        !definitions.contains(ENTRY_POINT),
        "install.sh invokes main more than once; this harness would still run it"
    );

    let script = format!(
        "{definitions}\nNEXUS_CONFIG={config}\n{body}\n",
        config = shell_quote(&config_dir.to_string_lossy()),
    );

    let output = Command::new("bash").arg("-c").arg(&script).output().expect("bash should run");
    assert!(
        output.status.success(),
        "installer function failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// An operator's existing settings must survive an upgrade untouched, and
/// settings the new version understands must appear.
#[test]
fn merging_adds_new_keys_and_keeps_existing_values() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.env");
    std::fs::write(
        &config,
        "# Nexus Node Configuration\n\
         GRPC_BIND=0.0.0.0:8080\n\
         AUTH_ENABLED=true\n\
         AUTH_PASSWORD=hunter2\n",
    )
    .unwrap();

    run_installer_fn(dir.path(), "merge_config");
    let merged = std::fs::read_to_string(&config).unwrap();

    // Nothing the operator set was disturbed — least of all the password.
    assert!(
        merged.contains("AUTH_PASSWORD=hunter2"),
        "config: {}",
        merged
    );
    assert!(merged.contains("GRPC_BIND=0.0.0.0:8080"));
    assert!(merged.contains("AUTH_ENABLED=true"));

    // And the settings this version added are now visible.
    assert!(
        merged.contains("UPDATE_CHANNEL=stable"),
        "new keys were not added: {}",
        merged
    );
}

/// Running the merge twice must change nothing the second time, since it runs
/// on every single update.
#[test]
fn merging_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.env");
    std::fs::write(&config, "GRPC_BIND=0.0.0.0:8080\n").unwrap();

    run_installer_fn(dir.path(), "merge_config");
    let once = std::fs::read_to_string(&config).unwrap();
    run_installer_fn(dir.path(), "merge_config");
    let twice = std::fs::read_to_string(&config).unwrap();

    assert_eq!(once, twice, "a second merge rewrote the config");
}

/// A value the operator changed is theirs. Resetting it to the default on
/// upgrade would silently undo a deliberate decision — switching a node off
/// the channel it was put on, for instance.
#[test]
fn merging_never_resets_a_changed_value() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.env");
    std::fs::write(&config, "UPDATE_CHANNEL=main\n").unwrap();

    run_installer_fn(dir.path(), "merge_config");
    let merged = std::fs::read_to_string(&config).unwrap();

    assert!(merged.contains("UPDATE_CHANNEL=main"), "config: {}", merged);
    assert!(
        !merged.contains("UPDATE_CHANNEL=stable"),
        "the operator's channel was overwritten: {}",
        merged
    );
}

/// A key the operator commented out was commented out on purpose.
#[test]
fn merging_leaves_commented_out_keys_alone() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.env");
    std::fs::write(&config, "# UPDATE_CHANNEL=main\n").unwrap();

    run_installer_fn(dir.path(), "merge_config");
    let merged = std::fs::read_to_string(&config).unwrap();

    assert_eq!(
        merged.matches("UPDATE_CHANNEL").count(),
        1,
        "the commented-out key was resurrected: {}",
        merged
    );
}

/// No config file means a fresh install, where `write_config` writes the whole
/// thing. The merge must not create a half-file that then looks complete.
#[test]
fn merging_does_nothing_without_an_existing_config() {
    let dir = tempfile::tempdir().unwrap();
    run_installer_fn(dir.path(), "merge_config");
    assert!(
        !dir.path().join("config.env").exists(),
        "the merge created a config where there was none"
    );
}

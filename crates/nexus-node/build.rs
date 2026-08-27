use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&["proto/node.proto"], &["proto"])?;

    stamp_build_identity();

    Ok(())
}

/// Record which source this binary was built from.
///
/// The crate version alone cannot answer "am I running the latest?" for a node
/// that tracks `main`: it changes only at a release, so every build between two
/// releases claims the same version. The commit does change, so it is what the
/// update check compares against the branch it follows.
///
/// Every value degrades to `unknown` rather than failing the build — the
/// installer builds from a shallow clone, and a source tarball has no git at
/// all. A node that cannot tell what it is running says so.
fn stamp_build_identity() {
    // Rebuild when the checked-out commit changes. Both files are consulted
    // because HEAD is a symref on a branch and only the ref file moves on a
    // commit; a detached HEAD moves HEAD itself.
    for path in [".git/HEAD", ".git/refs/heads/main"] {
        let path = std::path::Path::new("../..").join(path);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let short = sha.chars().take(12).collect::<String>();

    // Commit date, not build date: it identifies the source, and two rebuilds
    // of the same commit should not look like different versions.
    let commit_date = git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".to_string());

    // A dirty tree means the binary does not correspond to any commit, which
    // matters when an operator is wondering why their fix is not live.
    let dirty = git(&["status", "--porcelain"])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=NEXUS_GIT_SHA={}", sha);
    println!("cargo:rustc-env=NEXUS_GIT_SHA_SHORT={}", short);
    println!("cargo:rustc-env=NEXUS_GIT_COMMIT_DATE={}", commit_date);
    println!("cargo:rustc-env=NEXUS_GIT_DIRTY={}", dirty);
}

/// Run a git command in the workspace, returning trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).current_dir("../..").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

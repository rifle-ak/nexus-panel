# Contributing to Nexus Panel

Thanks for your interest in contributing! This guide covers the local setup and
the checks your change must pass.

## Prerequisites

- **Rust** (stable). Install via [rustup](https://rustup.rs).
- **protoc** (Protocol Buffers compiler) — **required**. The `containerd-client`
  build script generates gRPC bindings at compile time and fails without it.
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
  - Fedora: `sudo dnf install -y protobuf-compiler`
  - macOS: `brew install protobuf`
- **containerd** — only needed to exercise the real runtime. For development you
  can run the node with the mock runtime by setting `NEXUS_DEV_MODE=true`.

## Build, test, lint

Run these before opening a pull request — they mirror CI:

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`cargo fmt --all` (without `--check`) applies formatting. CI enforces a clean
`fmt`, a clippy run with **no warnings**, and `cargo deny check` for advisories.

## Running the node locally

```bash
NEXUS_DEV_MODE=true \
AUTH_ENABLED=true AUTH_PASSWORD=changeme \
WEB_BIND=127.0.0.1:3000 \
cargo run -p nexus-node --bin nexus-node
```

Then open <http://127.0.0.1:3000> and sign in with the password above.

## Pull requests

- Branch off `main` and keep PRs focused.
- Ensure CI is green and fill in the pull request template.
- Add or update tests for behavior changes.
- Match the style and altitude of the surrounding code; keep comments to
  constraints the code can't express on its own.

## Commit messages

Use clear, imperative subject lines (e.g. "Add schedule persistence"). Explain
the *why* in the body when it isn't obvious from the diff.

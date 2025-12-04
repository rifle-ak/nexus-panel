#!/bin/bash
set -e

echo "🚀 Nexus Panel - Quick Start"
echo ""

# Check if Rust is installed
if ! command -v cargo &> /dev/null; then
    echo "📦 Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source $HOME/.cargo/env
    echo "✅ Rust installed"
else
    echo "✅ Rust already installed"
fi

echo ""
echo "🔨 Building project..."
cargo build --release

echo ""
echo "✅ Build complete!"
echo ""
echo "📋 Available commands:"
echo "   ./target/release/nexus-panel convert --input <egg.json> --output <config.yaml>"
echo "   ./target/release/nexus-panel import --input-dir <dir> --output-dir <dir>"
echo "   ./target/release/nexus-panel clone --repo <url> --output-dir <dir>"
echo "   ./target/release/nexus-panel validate --input <config.yaml>"
echo ""
echo "🧪 Test the importer:"
echo "   ./target/release/nexus-panel convert --input examples/rust-egg.json --output test.yaml"
echo ""
echo "📚 Documentation:"
echo "   README.md      - Full documentation"
echo "   SUMMARY.md     - What we built and why"
echo "   DEVELOPER.md   - Contributing guide"
echo "   ERROR_CODES.md - Error reference"
echo "   LOGGING.md     - Logging guide"
echo ""

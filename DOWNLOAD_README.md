# Game Panel - Download & Setup

## 📦 What's Included

This package contains a production-ready Pterodactyl egg importer with comprehensive error handling and logging.

### Core Components
- **game-config** - Native YAML config format with security enhancements
- **egg-importer** - Pterodactyl egg converter with security scanning
- **CLI tool** - Command-line interface for conversion and validation

### Documentation
- **README.md** - Full project documentation
- **SUMMARY.md** - What we built and why it matters
- **DEVELOPER.md** - Contributing and development guide
- **ERROR_CODES.md** - Complete error reference (E001-E999)
- **LOGGING.md** - Logging guide and troubleshooting

### Examples
- **rust-egg.json** - Example Pterodactyl egg
- **rust-config.yaml** - Converted native config

## 🚀 Quick Start

### 1. Extract the archive

**Linux/Mac:**
```bash
tar -xzf game-panel.tar.gz
cd game-panel
```

**Windows:**
Extract `game-panel.zip` and open terminal in the folder.

### 2. Run setup script (Linux/Mac)

```bash
chmod +x setup.sh
./setup.sh
```

This will:
- Install Rust if needed
- Build the project in release mode
- Show you all available commands

### 3. Manual setup (Windows or if script fails)

```bash
# Install Rust from https://rustup.rs/
# Then build:
cargo build --release
```

### 4. Test it out

```bash
# Convert the example egg
./target/release/game-panel convert \
    --input examples/rust-egg.json \
    --output test.yaml

# Validate the output
./target/release/game-panel validate --input test.yaml
```

## 📋 CLI Commands

### Convert single egg
```bash
game-panel convert --input <egg.json> --output <config.yaml>
```

### Bulk import directory
```bash
game-panel import --input-dir ./eggs --output-dir ./configs
```

### Clone and import from GitHub
```bash
game-panel clone \
    --repo https://github.com/parkervcp/eggs \
    --output-dir ./configs
```

### Validate config
```bash
game-panel validate --input <config.yaml>
```

## 🔍 Error Handling

All errors include:
- **Error code** (E001-E999) for easy reference
- **Detailed description** of what went wrong
- **File/line context** when applicable
- **Solutions** with copy-paste commands

Example error:
```
ERROR [E101] Failed to parse Pterodactyl egg

File: /path/to/egg.json (line 42, column 15)

Problem: Invalid JSON syntax - expected comma

Solutions:
  1. Validate JSON: cat egg.json | jq
  2. Check syntax at: https://jsonlint.com
  3. Look for missing commas or quotes

Context:
  40 |     "name": "Test Server"
  41 |     "author": "support@pterodactyl.io"
  42 |     "description": "A test server"
       |                                    ^ expected comma here
```

See **ERROR_CODES.md** for complete reference.

## 📊 Logging

Set log level with `RUST_LOG` environment variable:

```bash
# Debug level (verbose)
RUST_LOG=debug game-panel convert --input egg.json

# Info level (default)
RUST_LOG=info game-panel convert --input egg.json

# Warning level (quiet)
RUST_LOG=warn game-panel convert --input egg.json
```

Logs include:
- Timestamps
- Log levels (DEBUG/INFO/WARN/ERROR)
- Component names
- File/line numbers
- Structured context

See **LOGGING.md** for complete guide.

## 🎯 Next Steps

### Import Parker's eggs (200+ games)
```bash
./target/release/game-panel clone \
    --repo https://github.com/parkervcp/eggs \
    --output-dir ./configs \
    --continue-on-error
```

### Create your own eggs
```bash
# Copy an example
cp examples/rust-egg.json my-game.json

# Edit it
vim my-game.json

# Convert and validate
./target/release/game-panel convert --input my-game.json
./target/release/game-panel validate --input my-game.yaml
```

### Contribute
See **DEVELOPER.md** for:
- Adding new game types
- Adding validation rules
- Adding security checks
- Testing and debugging

## 🐛 Troubleshooting

### Build fails
```bash
# Update Rust
rustup update

# Clean and rebuild
cargo clean
cargo build --release
```

### Permission denied
```bash
# Make setup script executable
chmod +x setup.sh

# Fix binary permissions
chmod +x target/release/game-panel
```

### Conversion errors
Check **ERROR_CODES.md** for your specific error code (E###).

## 📚 Documentation

- **README.md** - Full documentation and architecture
- **SUMMARY.md** - What we built, why it matters, roadmap
- **DEVELOPER.md** - Contributing guide, code structure
- **ERROR_CODES.md** - All error codes with solutions
- **LOGGING.md** - Logging guide and troubleshooting

## 🤝 Support

- Check **ERROR_CODES.md** first for solutions
- Enable debug logging: `RUST_LOG=debug`
- Review **LOGGING.md** for troubleshooting tips
- Open an issue on GitHub with logs

## 🎉 What Makes This Special

- ✅ **Security-first**: Auto-adds firewall rules, drops privileges
- ✅ **Type-safe**: Rust catches bugs at compile time
- ✅ **Validated**: Configs can't be invalid
- ✅ **Backward compatible**: Imports all Pterodactyl eggs
- ✅ **Developer-friendly**: Clear errors, detailed logging
- ✅ **Production-ready**: Comprehensive error handling

Now go build something awesome! 🚀

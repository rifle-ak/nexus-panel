# Developer Quick Start

## Build and Run

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone <repo-url>
cd nexus-panel
cargo build --release

# Run the CLI
./target/release/nexus-panel --help
```

## Project Architecture

### Crates

**nexus-config** - Core config format
- `GameConfig` struct with all the bells and whistles
- Validation logic
- YAML serialization/deserialization
- Independent of Pterodactyl eggs

**egg-importer** - Pterodactyl egg importer
- `PterodactylEgg` struct for parsing JSON
- `EggConverter` for transforming eggs to GameConfig
- Security scanning
- Smart defaults based on game type

**nexus-panel** (bin) - CLI tool
- Convert, import, clone, validate commands
- Pretty output with colors and progress
- Error handling and logging

### Key Files

```
crates/nexus-config/src/lib.rs       # Native config format
crates/egg-importer/src/pterodactyl.rs   # Egg JSON parser
crates/egg-importer/src/converter.rs     # Conversion logic
src/main.rs                          # CLI tool
```

## Adding a New Game Type

1. **Add game detection** in `converter.rs`:
```rust
fn detect_game_type(&self, egg: &PterodactylEgg) -> String {
    let searchable = format!("{} {}", egg.name, description);
    
    if searchable.contains("valheim") {
        "valheim".to_string()
    } else {
        "generic".to_string()
    }
}
```

2. **Add resource defaults** in `convert_resources`:
```rust
let (cpu_min, cpu_max, mem_min, mem_max, disk_min) = match game.as_str() {
    "valheim" => (2000, 4000, "4Gi", "8Gi", "10Gi"),
    _ => (1000, 2000, "1Gi", "2Gi", "5Gi"),
};
```

## Adding New Validation Rules

1. **Add rule variant** to `ValidationRule` enum in `nexus-config/src/lib.rs`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValidationRule {
    Port { range: (u16, u16) },
    Regex { pattern: String },
    Email,  // New rule
    // ...
}
```

2. **Parse the rule** in `converter.rs`:
```rust
fn parse_validation_rules(&self, rules: &str) -> Result<Vec<ValidationRule>> {
    for rule in rules.split('|') {
        if rule == "email" {
            parsed_rules.push(ValidationRule::Email);
        }
    }
}
```

## Adding Security Checks

Add patterns to `scan_egg_security` in `converter.rs`:
```rust
let dangerous_patterns = vec![
    (r"rm\s+-rf\s+/", "Dangerous: recursive root deletion"),
    (r"chmod\s+777", "Warning: overly permissive permissions"),
    (r"your_pattern_here", "Your warning message"),
];
```

## Testing

```bash
# Run all tests
cargo test

# Run with logging
RUST_LOG=debug cargo run -- convert --input test.json

# Test specific crate
cargo test -p nexus-config
cargo test -p egg-importer

# Format code
cargo fmt

# Lint
cargo clippy
```

## Creating Test Eggs

Put test eggs in `examples/`:
```bash
# Test conversion
cargo run -- convert \
    --input examples/your-game.json \
    --output examples/your-game.yaml

# Validate output
cargo run -- validate --input examples/your-game.yaml
```

## Common Tasks

### Import Parker's Eggs
```bash
# Clone the eggs repo
git clone https://github.com/parkervcp/eggs

# Import all eggs
cargo run --release -- import \
    --input-dir eggs \
    --output-dir configs \
    --continue-on-error

# Check for errors
grep "❌" import.log
```

### Debug Conversion Issues
```bash
# Run with debug logging
RUST_LOG=debug cargo run -- convert \
    --input problematic-egg.json \
    --output test.yaml

# Check the logs for warnings
```

### Add a New Lifecycle Hook Type
```rust
// In nexus-config/src/lib.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LifecycleAction {
    Download { url: String, condition: Option<String> },
    Execute { command: String, ... },
    WaitForFile { path: String, timeout: String }, // New
}
```

## Code Style

- Use descriptive variable names
- Add doc comments for public APIs
- Use `Result<T>` for fallible operations
- Prefer `anyhow::Result` for simple error handling
- Use `thiserror` for custom error types
- Log at appropriate levels (debug, info, warn, error)

## Performance Tips

- Use `--release` for benchmarking
- Profile with `cargo flamegraph`
- Avoid unnecessary allocations
- Use `&str` instead of `String` when possible
- Batch file operations

## Next Development Phases

See SUMMARY.md for the roadmap. The immediate next steps are:

1. **Improve converter**:
   - Better shell script parsing
   - Handle complex startup commands
   - Support multi-image eggs
   - Add more game types

2. **Wings daemon** (Rust):
   - Container orchestration
   - XDP firewall integration
   - Resource management
   - Health checks

3. **Panel API**:
   - REST/gRPC endpoints
   - Authentication
   - Server management
   - File operations

4. **Marketplace integration**:
   - Adapter system
   - Umod/Codefling/Lone.Design
   - Search and install
   - Auto-updates

## Questions?

Check the README.md and SUMMARY.md for more details.

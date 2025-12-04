# Error Handling & Diagnostics - Complete Guide

## What We Built

A comprehensive error handling and diagnostics system that makes troubleshooting **dummy-proof**. Every error tells you exactly what went wrong and how to fix it.

## Features

### 1. Rich Error Messages with Solutions

**Before (typical error):**
```
Error: No such file or directory (os error 2)
```

**After (our error):**
```
Failed to read file: /path/to/egg.json

💡 Solution:
  • Check if the file exists: ls -la /path/to/egg.json
  • Check file permissions: chmod 644 /path/to/egg.json
  • Verify you have read access to the directory

Error: No such file or directory (os error 2)
```

### 2. System Diagnostics Command

Check everything before you start:

```bash
game-panel diagnose
```

**Checks:**
- ✅ Operating System & Architecture
- ✅ System Resources (CPU, Memory, Disk)
- ✅ Required Commands (git, curl, docker, jq, yamllint)
- ✅ Rust Toolchain (rustc, cargo)
- ✅ File System Permissions
- ✅ Network Connectivity
- ✅ Container Runtime Status

**Output:**
```
🔍 System Diagnostics
================================================================================

✅ System Information
  ℹ Operating System: linux (x86_64)
  ℹ System Memory: 9.0Gi available
  ℹ CPU Cores: 4 cores available

⚠️ Required Commands
  ✓ git: Found at /usr/bin/git
  ✓ curl: Found at /usr/bin/curl
  ⚠ docker: Not found
    💡 Install docker: See https://docs.docker.com/engine/install/

✅ Rust Toolchain
  ✓ Rust Compiler: rustc 1.91.1
  ✓ Cargo: cargo 1.91.1

✅ File System
  ✓ Can read /home/user/project
  ✓ Can write to current directory
  ℹ Disk Space: 9.8G available (1% used)

⚠️ Network
  ⚠ Network may not be accessible
    💡 Check your internet connection
  ✓ GitHub Access: HTTP 200

================================================================================
📊 Summary: 16 checks, 4 warnings
```

**JSON Output for Automation:**
```bash
game-panel diagnose --format json > diagnostics.json
```

### 3. Error Codes with Documentation

Every error has a code (E###) you can look up:

```
Error E001: File Read Error
Error E101: Invalid Pterodactyl Egg JSON
Error E301: Security Issue
Error E901: Conversion Error
```

Full reference: `ERROR_CODES.md`

### 4. Improved Validation

**Before:**
```
Error: CPU min cannot be greater than max
```

**After:**
```
Config validation failed: Rust Server

❌ Validation Errors:
  • CPU min (4000) cannot be greater than max (2000)
  • Invalid memory format for 'min': '4GB'. Use format like '512Mi', '4Gi', '8Gi'
  • Variable 'RCON_PASSWORD': required but has no default value and is not user editable
  • Port 'game_port': malformed template variable in '{{SERVER_PORT'
  • Container image 'ghcr.io/pterodactyl/yolks' should include a tag (e.g., ghcr.io/pterodactyl/yolks:latest)

💡 Solution:
  Fix the above issues and re-run validation
```

### 5. Security Scanning with Context

When converting eggs:

```
📦 Converting egg: rust-egg.json
INFO Converting egg: Rust
INFO Scanning egg for security issues
WARN Security issue in installation script: Dangerous: piping curl to bash
✅ Converted to: rust-config.yaml
   Game: rust
   Variables: 17
   Ports: 2
   🔒 Security: Scanned and hardened
```

### 6. Validation Warnings

Not just errors - helpful warnings too:

```bash
game-panel validate --input config.yaml

✅ Config is valid!

⚠️ Warnings:
   • No health check configured
   • Variable 'ADMIN_PASSWORD' looks like a secret but is not marked as secret
   • No firewall rules configured (consider adding rate limiting)
```

## Usage Examples

### Pre-Flight Check

Before doing anything:
```bash
# Check if system is ready
game-panel diagnose

# Fix any failed checks
# Re-run until all green
```

### Converting with Full Logging

```bash
# Enable debug logging
RUST_LOG=debug game-panel convert \
  --input rust-egg.json \
  --output rust-config.yaml

# You'll see:
# - Security scanning details
# - Variable detection logic
# - Port analysis
# - Resource assignments
# - Every decision made
```

### Validating Configs

```bash
# Validate before deployment
game-panel validate --input config.yaml

# Check exit code
echo $?  # 0 = success, 1 = failure
```

### Troubleshooting Failed Conversions

```bash
# If conversion fails:
RUST_LOG=debug game-panel convert --input problematic-egg.json 2>&1 | tee debug.log

# Check the debug log for details
grep -i "error\|warn" debug.log

# Look up error code
cat ERROR_CODES.md | grep "E901"
```

### Automated Deployment Checks

```bash
#!/bin/bash
# Pre-deployment checks

echo "Running diagnostics..."
if ! game-panel diagnose; then
    echo "❌ System checks failed!"
    exit 1
fi

echo "Validating configs..."
for config in configs/*.yaml; do
    if ! game-panel validate --input "$config"; then
        echo "❌ Validation failed: $config"
        exit 1
    fi
done

echo "✅ All checks passed!"
```

## Error Categories

### File System (E001-E099)
- File not found
- Permission denied
- Disk full
- Directory doesn't exist

### Parsing (E101-E199)
- Invalid JSON
- Malformed YAML
- Missing required fields
- Bad syntax

### Validation (E201-E299)
- Resource limits invalid
- Missing required fields
- Invalid field values
- Template variable errors

### Security (E301-E399)
- Dangerous commands detected
- Overly permissive settings
- Missing security hardening
- Suspicious patterns

### Container (E401-E499)
- Invalid image format
- Image not found
- Registry unreachable

### Resources (E501-E599)
- Invalid memory format
- Invalid CPU specification
- Invalid disk size

### Network (E601-E699)
- Invalid port range
- Malformed port config
- Protocol errors

### Git (E701-E799)
- Git not installed
- Clone failed
- Network issues

### System (E801-E899)
- Missing dependencies
- Insufficient permissions
- System requirements not met

### Conversion (E901-E998)
- Egg parsing failed
- Conversion logic error
- Unsupported egg format

## Logging Levels

Control output verbosity:

```bash
# No logs (errors only)
game-panel convert --input egg.json

# Info level (default)
RUST_LOG=info game-panel convert --input egg.json

# Debug level (detailed)
RUST_LOG=debug game-panel convert --input egg.json

# Trace level (everything)
RUST_LOG=trace game-panel convert --input egg.json

# Specific module
RUST_LOG=egg_importer=debug game-panel convert --input egg.json
```

## Best Practices

### 1. Always Run Diagnostics First
```bash
game-panel diagnose
```

### 2. Enable Debug Logging for Issues
```bash
RUST_LOG=debug game-panel command 2>&1 | tee debug.log
```

### 3. Validate Before Deployment
```bash
game-panel validate --input config.yaml
```

### 4. Check Exit Codes in Scripts
```bash
if ! game-panel validate --input config.yaml; then
    echo "Validation failed!"
    exit 1
fi
```

### 5. Save Diagnostics for Support
```bash
game-panel diagnose --format json > diagnostics.json
# Include this when reporting issues
```

## Integration with Panel (Future)

When the panel is built, these features will be exposed via API:

```
GET  /api/diagnostics           # Run system diagnostics
GET  /api/logs/:server_id       # Get server logs
POST /api/validate              # Validate config before save
GET  /api/errors/:code          # Get error documentation
```

Panel UI will show:
- Real-time diagnostic status
- Pretty-printed error messages with solutions
- One-click fixes for common issues
- Searchable error code reference

## Files Added

```
game-panel/
├── crates/game-config/src/
│   ├── errors.rs           # Rich error types with solutions
│   └── diagnostics.rs      # System diagnostic checks
├── ERROR_CODES.md          # Complete error reference
└── LOGGING.md              # This file
```

## Testing the Features

### Test Error Messages
```bash
# File not found
./target/release/game-panel convert --input nonexistent.json

# Invalid YAML
echo "bad: yaml: syntax" > bad.yaml
./target/release/game-panel validate --input bad.yaml

# Invalid egg JSON
echo "{invalid json}" > bad.json
./target/release/game-panel convert --input bad.json
```

### Test Diagnostics
```bash
# Text output
./target/release/game-panel diagnose

# JSON output
./target/release/game-panel diagnose --format json | jq .
```

### Test Validation
```bash
# Valid config
./target/release/game-panel validate --input examples/rust-config.yaml

# See warnings
# Create a config without health checks or firewall rules
```

## Why This Matters

**Before:**
- Users get cryptic errors
- No idea what went wrong
- Have to ask for help
- Frustration and wasted time

**After:**
- Every error explains itself
- Clear actionable solutions
- Self-service troubleshooting
- Users can fix their own issues

**Result:**
- Reduced support burden
- Faster problem resolution
- Better user experience
- Professional polish

## Next Steps

When building the panel:
1. Expose diagnostics via API
2. Create UI for error messages
3. Add log aggregation
4. Build troubleshooting wizard
5. Integrate with monitoring

The foundation is rock solid. Error handling and diagnostics that actually help users fix their own problems.

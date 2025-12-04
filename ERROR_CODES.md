# Error Codes Reference

All errors in Nexus Panel include an error code (E###) for easy reference and troubleshooting.

## File System Errors (E001-E099)

### E001 - File Read Error
**Problem:** Cannot read the specified file.

**Solutions:**
```bash
# Check if file exists
ls -la /path/to/file

# Check permissions
chmod 644 /path/to/file

# Verify directory access
ls -ld /path/to/directory
```

### E002 - File Write Error
**Problem:** Cannot write to the specified file.

**Solutions:**
```bash
# Check if directory exists
mkdir -p /path/to/directory

# Check disk space
df -h

# Check write permissions
ls -ld /path/to/directory
chmod 755 /path/to/directory
```

### E003 - Directory Not Found
**Problem:** The specified directory doesn't exist.

**Solutions:**
```bash
# Create directory
mkdir -p /path/to/directory

# Verify path is correct
ls -la /path/to/
```

## Parsing Errors (E101-E199)

### E101 - Invalid Pterodactyl Egg JSON
**Problem:** The egg JSON file is malformed or not a valid Pterodactyl egg.

**Solutions:**
```bash
# Validate JSON syntax
cat egg.json | jq .

# Check for common issues
# - Trailing commas
# - Missing quotes
# - Unclosed brackets

# Verify it's a valid egg (has required fields)
jq '.meta, .startup, .variables' egg.json

# Download fresh copy from source
git clone https://github.com/parkervcp/eggs
```

### E102 - Invalid YAML Config
**Problem:** The native config YAML is malformed.

**Solutions:**
```bash
# Validate YAML syntax
yamllint config.yaml

# Common issues:
# - Incorrect indentation (must be 2 spaces)
# - Missing colons after keys
# - Unquoted special characters (: [ ] { })

# Compare with example
diff config.yaml examples/rust-config.yaml
```

## Validation Errors (E201-E299)

### E201 - Validation Error
**Problem:** Config failed validation checks.

**What to check:**
- Required fields are present
- Resource limits are valid (min < max)
- Memory/CPU formats are correct
- Port ranges are valid
- Template variables are properly formatted

**Solutions:**
See the specific validation errors listed. Common fixes:
```yaml
# Memory format
resources:
  memory:
    min: 4Gi  # Correct
    max: 4GB  # Wrong - use Gi not GB

# CPU format
resources:
  cpu:
    min: 2000  # Correct (2 cores)
    max: 200   # Wrong - max must be >= min

# Port template
networking:
  ports:
    - internal: "{{SERVER_PORT}}"  # Correct
    - internal: "{{SERVER_PORT"     # Wrong - missing }}
```

### E202 - Missing Required Field
**Problem:** A required configuration field is missing.

**Solution:** Add the missing field. Check examples/ directory for reference.

### E203 - Invalid Field Value
**Problem:** A field has an invalid value.

**Solution:** Update the field to match the expected format. Error message includes examples.

## Security Errors (E301-E399)

### E301 - Security Issue
**Problem:** Potential security issue detected in configuration.

**Common Issues:**
```bash
# Dangerous commands detected
rm -rf /            # Recursive deletion
chmod 777 file      # Overly permissive
curl url | bash     # Piping to shell
eval $variable      # Eval with variables

# Solutions:
# - Use structured lifecycle hooks instead of shell scripts
# - Limit file permissions (644 for files, 755 for dirs)
# - Download and verify before executing
# - Avoid eval, use proper variable expansion
```

**Best Practices:**
- Drop ALL capabilities, only add what's needed
- Use seccomp profiles
- Enable no_new_privileges
- Add XDP firewall rules
- Mark secrets properly

## Container Errors (E401-E499)

### E401 - Invalid Docker Image
**Problem:** Docker image specification is invalid.

**Solutions:**
```bash
# Check image exists
docker pull ghcr.io/pterodactyl/yolks:rust

# Verify registry is accessible
ping ghcr.io

# Use correct format
# Format: registry/repository:tag
# Example: ghcr.io/pterodactyl/yolks:rust

# Don't forget the tag
# Bad:  ghcr.io/pterodactyl/yolks
# Good: ghcr.io/pterodactyl/yolks:latest
```

## Resource Errors (E501-E599)

### E501 - Invalid Resource Spec
**Problem:** Resource specification format is incorrect.

**Solutions:**
```yaml
# Memory format (use Mi, Gi, Ti)
resources:
  memory:
    min: 4Gi       # Correct
    min: 4GB       # Wrong - use Gi
    min: 4000Mi    # Correct (same as ~4Gi)

# CPU format (millicores: 1000 = 1 core)
resources:
  cpu:
    min: 2000      # Correct (2 cores)
    max: 4000      # Correct (4 cores)
    min: 2         # Wrong - use millicores

# Disk format (use Mi, Gi, Ti)
resources:
  disk:
    min: 20Gi      # Correct
```

## Network Errors (E601-E699)

### E601 - Invalid Port
**Problem:** Port configuration is invalid.

**Solutions:**
```yaml
# Port must be 1024-65535
networking:
  ports:
    - name: game_port
      internal: "{{SERVER_PORT}}"  # Template variable
      protocol: udp                 # tcp, udp, or both
      required: true

# Common issues:
# - Port < 1024 (requires elevated privileges)
# - Port > 65535 (invalid)
# - Missing protocol
# - Malformed template variable
```

## Git Errors (E701-E799)

### E701 - Git Error
**Problem:** Git command failed.

**Solutions:**
```bash
# Install git
apt-get install git              # Ubuntu/Debian
brew install git                 # macOS
dnf install git                  # Fedora
pacman -S git                    # Arch

# Verify installation
which git
git --version

# Check network
ping github.com

# Verify repository URL
git ls-remote https://github.com/parkervcp/eggs
```

## System Errors (E801-E899)

### E801 - System Requirements Not Met
**Problem:** Required system dependencies are missing.

**Solution:**
```bash
# Run diagnostics
nexus-panel diagnose

# Install missing dependencies based on your OS
# See output of diagnose command for specific instructions
```

### E802 - Insufficient Permissions
**Problem:** Don't have necessary permissions.

**Solutions:**
```bash
# Run with sudo (if appropriate)
sudo nexus-panel ...

# Check file ownership
ls -la /path/to/file
chown user:group /path/to/file

# Add user to required groups
sudo usermod -aG docker $USER
# Log out and back in for group changes to take effect
```

## Conversion Errors (E901-E998)

### E901 - Conversion Error
**Problem:** Failed to convert Pterodactyl egg to native format.

**Solutions:**
```bash
# Run with debug logging
RUST_LOG=debug nexus-panel convert --input egg.json

# Check egg format
jq . egg.json

# Validate egg has required fields
jq '.meta, .startup, .variables, .scripts' egg.json

# Try without security scanning
nexus-panel convert --input egg.json --no-security-scan

# Report issue if egg is from official source
```

## Generic Errors (E999)

### E999 - Unexpected Error
**Problem:** An unexpected error occurred.

**Solutions:**
```bash
# Run diagnostics
nexus-panel diagnose

# Run with debug logging
RUST_LOG=debug nexus-panel ...

# Check system requirements
nexus-panel diagnose

# Report the issue
# Include:
# - Full error message
# - Command you ran
# - Output of diagnostics
```

---

## Quick Troubleshooting Checklist

Before asking for help, try these:

1. **Run Diagnostics:**
   ```bash
   nexus-panel diagnose
   ```

2. **Enable Debug Logging:**
   ```bash
   RUST_LOG=debug nexus-panel [command]
   ```

3. **Validate Your Files:**
   ```bash
   # For eggs
   cat egg.json | jq .
   
   # For configs
   nexus-panel validate --input config.yaml
   ```

4. **Check Permissions:**
   ```bash
   ls -la /path/to/file
   chmod 644 file        # For data files
   chmod 755 directory   # For directories
   ```

5. **Check Disk Space:**
   ```bash
   df -h
   ```

6. **Verify Network:**
   ```bash
   ping github.com
   curl -I https://github.com
   ```

## Getting Help

If you're still stuck:

1. Run diagnostics: `nexus-panel diagnose --format json > diagnostics.json`
2. Include the error code and full error message
3. Share what you've tried
4. Provide your OS and version
5. Open an issue on GitHub with all the above info

---

## Contributing

Found a common error pattern not documented here? Submit a PR!

Error codes are defined in: `crates/nexus-config/src/errors.rs`

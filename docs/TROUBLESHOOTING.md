# Troubleshooting Guide

Common issues and solutions.

## Diagnostics Command

Run system diagnostics:

```bash
./target/release/nexus-panel diagnose
```

This checks:
- OS and kernel version
- Available resources (CPU, memory, disk)
- Required commands (containerd, runc, ctr)
- Rust toolchain
- File permissions
- Network connectivity
- Container runtime status

## Error Codes

### File System Errors (E001-E099)

| Code | Problem | Solution |
|------|---------|----------|
| E001 | File not found | Check file path exists |
| E002 | Permission denied | Check file permissions, run as root if needed |
| E003 | Directory not found | Create parent directories |
| E004 | Disk full | Free up disk space |
| E005 | Path traversal | Use absolute paths without `..` |

### Parsing Errors (E101-E199)

| Code | Problem | Solution |
|------|---------|----------|
| E101 | Invalid JSON | Validate JSON syntax |
| E102 | Invalid YAML | Validate YAML syntax |
| E103 | Missing required field | Add missing field to config |
| E104 | Invalid field value | Check field value constraints |
| E105 | Unsupported format | Use supported config format |

### Container Errors (E201-E299)

| Code | Problem | Solution |
|------|---------|----------|
| E201 | Container not found | Check container ID |
| E202 | Container already exists | Use different ID or delete existing |
| E203 | Image not found | Pull image first with `ctr images pull` |
| E204 | Container start failed | Check logs, verify image and config |
| E205 | Container stop timeout | Container may be hung, use force stop |

### Network Errors (E301-E399)

| Code | Problem | Solution |
|------|---------|----------|
| E301 | Port already in use | Choose different port or stop conflicting service |
| E302 | Network unreachable | Check network configuration |
| E303 | DNS resolution failed | Check DNS settings |
| E304 | Connection refused | Check target service is running |
| E305 | TLS handshake failed | Verify certificates |

### Runtime Errors (E401-E499)

| Code | Problem | Solution |
|------|---------|----------|
| E401 | Containerd unavailable | Start containerd service |
| E402 | Socket permission denied | Check socket permissions, add user to group |
| E403 | Namespace not found | Create namespace with `ctr namespace create` |
| E404 | Resource limit exceeded | Increase limits or free resources |
| E405 | OOM killed | Increase memory limit |

## Common Issues

### Containerd Connection Failed

**Symptoms:**
```
Error: Failed to connect to Containerd at /run/containerd/containerd.sock
```

**Solutions:**

1. Check containerd is running:
   ```bash
   sudo systemctl status containerd
   sudo systemctl start containerd
   ```

2. Check socket exists:
   ```bash
   ls -la /run/containerd/containerd.sock
   ```

3. Check permissions:
   ```bash
   sudo chmod 666 /run/containerd/containerd.sock
   # Or add user to containerd group
   sudo usermod -aG containerd $USER
   ```

4. Test connection:
   ```bash
   sudo ctr version
   ```

### Port Already in Use

**Symptoms:**
```
Error: Address already in use: 0.0.0.0:8080
```

**Solutions:**

1. Find process using port:
   ```bash
   sudo lsof -i :8080
   sudo netstat -tlnp | grep 8080
   ```

2. Kill the process:
   ```bash
   sudo kill <PID>
   ```

3. Use different port:
   ```bash
   export GRPC_BIND=0.0.0.0:8081
   ```

### Permission Denied

**Symptoms:**
```
Error: Permission denied: /var/lib/nexus-node
```

**Solutions:**

1. Create directory with correct ownership:
   ```bash
   sudo mkdir -p /var/lib/nexus-node
   sudo chown $USER:$USER /var/lib/nexus-node
   ```

2. Run as root:
   ```bash
   sudo ./nexus-node
   ```

3. Check SELinux/AppArmor:
   ```bash
   sudo setenforce 0  # Temporarily disable SELinux
   sudo aa-complain nexus-node  # AppArmor complain mode
   ```

### Image Not Found

**Symptoms:**
```
Error: Image not found: docker.io/itzg/minecraft-server:latest
```

**Solutions:**

1. Pull image manually:
   ```bash
   sudo ctr -n nexus-panel images pull docker.io/itzg/minecraft-server:latest
   ```

2. Check available images:
   ```bash
   sudo ctr -n nexus-panel images list
   ```

3. Verify image name in config matches pulled image exactly.

### Container Start Failed

**Symptoms:**
```
Error: Failed to start container: exit code 1
```

**Solutions:**

1. Check container logs:
   ```bash
   grpcurl -plaintext -d '{"container_id":"<id>","tail":100}' \
     localhost:8080 nexus.node.v1.NodeService/StreamLogs
   ```

2. Verify config:
   ```bash
   ./nexus-panel validate --input config.yaml
   ```

3. Check resource availability:
   ```bash
   free -h
   df -h
   ```

4. Try running the image directly:
   ```bash
   sudo ctr -n nexus-panel run --rm docker.io/image:tag test-run
   ```

### TLS/mTLS Errors

**Symptoms:**
```
Error: TLS handshake failed: certificate verify failed
```

**Solutions:**

1. Verify certificate chain:
   ```bash
   openssl verify -CAfile ca.crt server.crt
   ```

2. Check certificate dates:
   ```bash
   openssl x509 -in server.crt -noout -dates
   ```

3. Verify key matches certificate:
   ```bash
   openssl x509 -in server.crt -noout -modulus | md5sum
   openssl rsa -in server.key -noout -modulus | md5sum
   # Hashes should match
   ```

### Rate Limit Exceeded

**Symptoms:**
```
Error: Rate limit exceeded
Status: 429 Too Many Requests
```

**Solutions:**

1. Wait for rate limit reset (check `Retry-After` header)

2. Increase rate limit:
   ```bash
   export RATE_LIMIT_PER_CLIENT_RPS=200
   ```

3. Implement client-side throttling

### Backup Failed

**Symptoms:**
```
Error: Backup failed: disk full
```

**Solutions:**

1. Check disk space:
   ```bash
   df -h /var/lib/nexus-node
   ```

2. Clean old backups:
   ```bash
   # List backups
   ls -la /var/lib/nexus-node/backups/<container-id>/

   # Delete old backups via API or manually
   ```

3. Use exclude patterns to reduce backup size:
   ```bash
   # Exclude logs, cache, temp files
   grpcurl -plaintext -d '{
     "container_id": "...",
     "exclude_paths": ["/logs", "/cache", "*.tmp"]
   }' localhost:8080 nexus.node.v1.NodeService/CreateBackup
   ```

## Logging

### Enable Debug Logging

```bash
RUST_LOG=debug ./nexus-node
```

### Module-Specific Logging

```bash
RUST_LOG=nexus_node::grpc=debug,nexus_node::runtime=trace ./nexus-node
```

### Log to File

```bash
./nexus-node 2>&1 | tee /var/log/nexus-node.log
```

### Journald

```bash
# View logs
sudo journalctl -u nexus-node -f

# View errors only
sudo journalctl -u nexus-node -p err

# View last hour
sudo journalctl -u nexus-node --since "1 hour ago"
```

## Health Checks

### Basic Health

```bash
curl http://localhost:9090/health
```

### Detailed Health (via gRPC)

```bash
grpcurl -plaintext localhost:8080 nexus.node.v1.NodeService/HealthCheck
```

### Prometheus Metrics

```bash
curl http://localhost:9090/metrics | grep nexus_node
```

## Getting Help

1. Check this documentation
2. Search existing issues: https://github.com/rifle-ak/nexus-panel/issues
3. Open a new issue with:
   - Error message
   - Steps to reproduce
   - System information (`./nexus-panel diagnose`)
   - Relevant logs

#!/bin/bash
# Test log streaming via gRPC

set -e

echo "=== Nexus Node Log Streaming Test ==="
echo

# Check if grpcurl is available
if ! command -v grpcurl &> /dev/null; then
    echo "❌ grpcurl not found. Install with:"
    echo "   go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest"
    exit 1
fi

# Check if server is running
if ! grpcurl -plaintext localhost:8080 list >/dev/null 2>&1; then
    echo "❌ Nexus Node server not running on localhost:8080"
    echo "   Start it with: cargo run --bin nexus-node"
    exit 1
fi

echo "✓ Server is running"
echo

# Create a test container
echo "Creating test container..."
CONFIG_YAML=$(cat <<'EOF'
metadata:
  id: test-logs
  name: Log Streaming Test
  game: minecraft
  version: 1.0.0
  author: test@example.com
container:
  image: "itzg/minecraft-server:latest"
  environment: {}
resources:
  cpu: {min: 1000, max: 2000, shares: 1024}
  memory: {min: 1Gi, max: 2Gi, swap: 512Mi}
  disk: {min: 5Gi, io_priority: normal}
startup:
  command: "java"
  args: ["-jar", "server.jar"]
  working_dir: /home/container
  lifecycle: {pre_start: [], post_start: [], pre_stop: []}
networking:
  ports:
    - {name: game, internal: "25565", protocol: tcp, required: true}
variables: []
security:
  capabilities: {add: [], drop: []}
  firewall_rules: []
EOF
)

# Escape YAML for JSON
CONFIG_ESCAPED=$(echo "$CONFIG_YAML" | sed 's/\\/\\\\/g' | sed ':a;N;$!ba;s/\n/\\n/g' | sed 's/"/\\"/g')

RESPONSE=$(grpcurl -plaintext -d "{
  \"config_yaml\": \"$CONFIG_ESCAPED\",
  \"container_id\": \"test-log-stream\",
  \"auto_start\": true
}" localhost:8080 nexus.node.v1.NodeService/CreateContainer 2>&1)

if echo "$RESPONSE" | grep -q "container_id"; then
    echo "✓ Container created and started"
else
    echo "❌ Failed to create container:"
    echo "$RESPONSE"
    exit 1
fi
echo

# Stream logs (without follow - will get 10 lines and EOF)
echo "Streaming logs (no follow, will get 10 lines):"
echo "─────────────────────────────────────────────────────"

grpcurl -plaintext -d '{
  "container_id": "test-log-stream",
  "follow": false,
  "tail": 0
}' localhost:8080 nexus.node.v1.NodeService/StreamLogs | while read -r line; do
    # Parse JSON and extract the log line
    if echo "$line" | grep -q '"line"'; then
        LOG_LINE=$(echo "$line" | grep -o '"line":"[^"]*"' | cut -d'"' -f4)
        echo "$LOG_LINE"
    fi
done

echo "─────────────────────────────────────────────────────"
echo "✓ Log streaming completed"
echo

# Clean up
echo "Cleaning up..."
grpcurl -plaintext -d '{
  "container_id": "test-log-stream",
  "force": true
}' localhost:8080 nexus.node.v1.NodeService/DeleteContainer >/dev/null

echo "✓ Cleanup complete"
echo
echo "=== Test Completed Successfully ==="

#!/usr/bin/env bash
# run_local.sh — One-shot local single-GPU launcher.
#
# Starts the tinyllb in development mode, assuming a vLLM backend
# at http://localhost:8000 (or $TINYLLB_BACKEND_URL).
#
# Usage:
#   ./scripts/run_local.sh
#
# After the proxy starts, test with:
#   curl localhost:8080/v1/models
#   curl -X POST localhost:8080/v1/chat/completions \
#     -H "Content-Type: application/json" \
#     -d '{
#       "model": "meta-llama/Llama-3.2-1B-Instruct",
#       "messages": [{"role": "user", "content": "Hello"}]
#     }'

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BACKEND="${TINYLLB_BACKEND_URL:-http://localhost:8000}"
PROXY_PORT="${PORT:-8080}"
CONFIG_PATH="${CONFIG_PATH:-${REPO_ROOT}/config.yaml}"

echo "=== tinyllb (local dev) ==="
echo "Backend:  $BACKEND"
echo "Proxy:    0.0.0.0:${PROXY_PORT}"
echo "Config:   $CONFIG_PATH"
echo ""

# If config.yaml doesn't exist, create it from the example.
if [ ! -f "$CONFIG_PATH" ]; then
    echo "config.yaml not found at $CONFIG_PATH; copying config.example.yaml..."
    cp "${REPO_ROOT}/config.example.yaml" "$CONFIG_PATH"
fi

echo ""
echo "--- Starting proxy (Ctrl+C to stop) ---"
echo ""

export CONFIG_PATH
export PORT="$PROXY_PORT"

# If BACKEND differs from default, override via env.
if [ "$BACKEND" != "http://localhost:8000" ]; then
    export TINYLLB__BACKEND__URL="$BACKEND"
fi

cd "$REPO_ROOT"
exec cargo run --release

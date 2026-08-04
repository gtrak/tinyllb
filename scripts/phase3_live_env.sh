#!/usr/bin/env bash
# phase3_live_env.sh — Environment setup for Phase 3 live tests.
#
# Source this file before running live tests or benchmarks:
#   source scripts/phase3_live_env.sh
#
# Then run:
#   TINYLLB_LIVE_TESTS=1 cargo test --test phase3_live -- --ignored

set -euo pipefail

# Enable live test suite (removes #[ignore] gates).
export TINYLLB_LIVE_TESTS=1

# Backend URL — point to your running vLLM instance.
# Override with your own endpoint:
#   export TINYLLB_BACKEND_URL="http://your-vllm-host:8000"
export TINYLLB_BACKEND_URL="${TINYLLB_BACKEND_URL:-http://gary-agents:1234}"

echo "Phase 3 live environment configured:"
echo "  TINYLLB_LIVE_TESTS=$TINYLLB_LIVE_TESTS"
echo "  TINYLLB_BACKEND_URL=$TINYLLB_BACKEND_URL"
echo ""
echo "Run: TINYLLB_LIVE_TESTS=1 cargo test --test phase3_live -- --ignored"

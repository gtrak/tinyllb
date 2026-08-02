#!/usr/bin/env bash
# phase3_live_env.sh — Environment setup for Phase 3 live tests.
#
# Source this file before running live tests or benchmarks:
#   source scripts/phase3_live_env.sh
#
# Then run:
#   LLM_QDISC_LIVE_TESTS=1 cargo test --test phase3_live -- --ignored

set -euo pipefail

# Enable live test suite (removes #[ignore] gates).
export LLM_QDISC_LIVE_TESTS=1

# Backend URL — point to your running vLLM instance.
# Override with your own endpoint:
#   export LLM_QDISC_BACKEND_URL="http://your-vllm-host:8000"
export LLM_QDISC_BACKEND_URL="${LLM_QDISC_BACKEND_URL:-http://gary-agents:1234}"

echo "Phase 3 live environment configured:"
echo "  LLM_QDISC_LIVE_TESTS=$LLM_QDISC_LIVE_TESTS"
echo "  LLM_QDISC_BACKEND_URL=$LLM_QDISC_BACKEND_URL"
echo ""
echo "Run: LLM_QDISC_LIVE_TESTS=1 cargo test --test phase3_live -- --ignored"

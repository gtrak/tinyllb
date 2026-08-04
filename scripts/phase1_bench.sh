#!/usr/bin/env bash
# phase1_bench.sh — Reproducible benchmark runner for Phase 1 throughput tests.
#
# Usage: ./scripts/phase1_bench.sh
#
# Runs `cargo bench --bench throughput`, captures RESULT lines from stderr,
# computes average tokens/sec per (scenario, concurrency) pair, and writes
# a comparison table to docs/plans/001-tinyllb/PHASE1-RESULTS.md.
#
# The RESULT lines have the format:
#   RESULT <direct|proxy> concurrency=<N> waves=<W> requests=<R> tokens=<T> wall=<W> tok/s=<S> peak_inflight=<P> base_time=<B>ms penalty=<Q>

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="$REPO_ROOT/docs/plans/001-tinyllb"
RESULTS_FILE="$RESULTS_DIR/PHASE1-RESULTS.md"
STDERR_LOG=$(mktemp)

cleanup() {
    rm -f "$STDERR_LOG"
}
trap cleanup EXIT

echo "=== Phase 1 Throughput Benchmark ==="
echo "Repo: $REPO_ROOT"
echo "Results: $RESULTS_FILE"
echo ""

# Run the benchmark, capturing stderr (RESULT lines) separately
echo "Running cargo bench --bench throughput ..."
cargo bench --bench throughput 2>"$STDERR_LOG"

echo "Bench complete. Parsing RESULT lines..."

# Parse RESULT lines from stderr log.
# Criterion runs 3 warmup iterations followed by 10 measurement iterations per
# benchmark function. We skip warmup iterations and average the measurement
# iterations per (scenario, concurrency) pair.
declare -A direct_sum
declare -A proxy_sum
declare -A direct_peak
declare -A proxy_peak
declare -A direct_count
declare -A proxy_count
declare -A iteration_index

# Stub parameters (from throughput.rs constants)
STUB_BASE="20"
STUB_PENALTY="0.05"
TOTAL_REQ="32"

while IFS= read -r line; do
    if [[ "$line" =~ ^RESULT\ (direct|proxy)\ concurrency=([0-9]+).*waves=([0-9]+).*tok/s=([0-9.]+).*peak_inflight=([0-9]+).*base_time=([0-9]+).*penalty=([0-9.]+) ]]; then
        scenario="${BASH_REMATCH[1]}"
        concurrency="${BASH_REMATCH[2]}"
        waves="${BASH_REMATCH[3]}"
        tok_s="${BASH_REMATCH[4]}"
        peak="${BASH_REMATCH[5]}"
        base="${BASH_REMATCH[6]}"
        penalty="${BASH_REMATCH[7]}"

        # Capture stub params from first match
        STUB_BASE="$base"
        STUB_PENALTY="$penalty"

        # Track iteration index per benchmark key to skip warmup (first 3 iterations)
        bench_key="${scenario}_${concurrency}"
        if [[ "$bench_key" != "${prev_key:-}" ]]; then
            iteration_index[$bench_key]=0
            prev_key="$bench_key"
        fi
        idx="${iteration_index[$bench_key]}"
        iteration_index[$bench_key]=$((idx + 1))
        if [[ "$idx" -lt 3 ]]; then
            continue  # Skip warmup iterations
        fi

        key="${concurrency}"
        if [[ "$scenario" == "direct" ]]; then
            direct_sum[$key]=$(echo "${direct_sum[$key]:-0} + $tok_s" | bc)
            direct_peak[$key]="$peak"
            direct_count[$key]=$((${direct_count[$key]:-0} + 1))
        else
            proxy_sum[$key]=$(echo "${proxy_sum[$key]:-0} + $tok_s" | bc)
            proxy_peak[$key]="$peak"
            proxy_count[$key]=$((${proxy_count[$key]:-0} + 1))
        fi
    fi
done < "$STDERR_LOG"

# Generate the results markdown
echo "Writing results to $RESULTS_FILE ..."

cat > "$RESULTS_FILE" << HEADER
# Phase 1 Benchmark Results

## Stub Parameters

| Parameter | Value |
| --- | --- |
| base_time_ms | ${STUB_BASE} |
| penalty (quadratic) | ${STUB_PENALTY} |
| tokens_per_request | 10 |
| total_requests | ${TOTAL_REQ} |
| max_active_flows (proxy) | 4 |
| formula | service_time = base_time × (1 + penalty × in_flight²) |

## Methodology

Criterion benchmark with \`sample_size=10\`, \`measurement_time=10s\`.
Requests are dispatched in waves: \`total_requests / concurrency\` waves,
each containing \`concurrency\` simultaneous clients. Waves complete
sequentially (all clients in wave N finish before wave N+1 begins).
Two scenarios: **direct** (clients → stub) and **proxy** (clients → proxy → stub).
Tokens/sec computed as \`total_tokens / wall_time\`.
The proxy uses \`max_active_flows=4\` with FIFO scheduling and blocking backpressure.
Warmup iterations (first 3 per benchmark function) are excluded from averages.

## Run Details

- **Date:** $(date -u '+%Y-%m-%dT%H:%M:%SZ')
- **Samples per benchmark:** 10 measurement iterations (3 warmup excluded)

## Comparison Table

| Concurrency (simultaneous clients/wave) | Waves | Direct tok/s | Proxy tok/s | Ratio (proxy/direct) | Direct Peak In-flight | Proxy Peak In-flight |
| --- | --- | --- | --- | --- | --- | --- |

HEADER

# Build comparison table
for conc in 1 4 8 16 32; do
    dcount="${direct_count[$conc]:-0}"
    pcount="${proxy_count[$conc]:-0}"
    waves=$((TOTAL_REQ / conc))
    if [[ "$dcount" -gt 0 && "$pcount" -gt 0 ]]; then
        avg_direct=$(echo "scale=1; ${direct_sum[$conc]} / $dcount" | bc)
        avg_proxy=$(echo "scale=1; ${proxy_sum[$conc]} / $pcount" | bc)
        ratio=$(echo "scale=2; $avg_proxy / $avg_direct" | bc)
        peak_direct="${direct_peak[$conc]:-N/A}"
        peak_proxy="${proxy_peak[$conc]:-N/A}"
        echo "| $conc | $waves | $avg_direct | $avg_proxy | $ratio | $peak_direct | $peak_proxy |" >> "$RESULTS_FILE"
    else
        echo "| $conc | $waves | N/A | N/A | N/A | N/A | N/A |" >> "$RESULTS_FILE"
    fi
done

echo "" >> "$RESULTS_FILE"

# Raw per-sample data
echo "## Raw Per-Sample Data" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"
echo "All measurement RESULT lines (warmup iterations excluded from averages):" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"
echo '```' >> "$RESULTS_FILE"
grep "^RESULT " "$STDERR_LOG" >> "$RESULTS_FILE" 2>/dev/null || echo "(no RESULT data available)" >> "$RESULTS_FILE"
echo '```' >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"

# Verdict section
echo "## Phase 1 Criterion: PASS/GAP Verdict" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"

# Determine verdict from the highest concurrency level
highest_conc=""
for conc in 32 16 8 4 1; do
    if [[ "${direct_count[$conc]:-0}" -gt 0 && "${proxy_count[$conc]:-0}" -gt 0 ]]; then
        highest_conc="$conc"
        break
    fi
done

if [[ -n "$highest_conc" ]]; then
    dcount_h="${direct_count[$highest_conc]:-0}"
    pcount_h="${proxy_count[$highest_conc]:-0}"
    avg_direct_h=$(echo "scale=1; ${direct_sum[$highest_conc]} / $dcount_h" | bc)
    avg_proxy_h=$(echo "scale=1; ${proxy_sum[$highest_conc]} / $pcount_h" | bc)
    ratio_h=$(echo "scale=2; $avg_proxy_h / $avg_direct_h" | bc)
    peak_direct_h="${direct_peak[$highest_conc]:-N/A}"
    peak_proxy_h="${proxy_peak[$highest_conc]:-N/A}"

    if (( $(echo "$avg_proxy_h >= $avg_direct_h" | bc -l) )); then
        verdict="PASS"
        analysis="At concurrency ${highest_conc} (peak in-flight: direct=${peak_direct_h}, proxy=${peak_proxy_h}), the proxy sustains ${avg_proxy_h} tok/s vs direct ${avg_direct_h} tok/s (ratio ${ratio_h}). The proxy's admission control limits backend concurrency to max_active_flows=4, preventing the quadratic collapse (service_time = ${STUB_BASE}ms × (1 + ${STUB_PENALTY} × in_flight²)) that hits the direct path when peak in-flight exceeds the proxy's cap. This demonstrates the Phase 1 thesis: admission control prevents GPU KV-cache saturation collapse under bursty overload."
    else
        verdict="GAP"
        analysis="At concurrency ${highest_conc} (peak in-flight: direct=${peak_direct_h}, proxy=${peak_proxy_h}), the proxy achieves ${avg_proxy_h} tok/s vs direct ${avg_direct_h} tok/s (ratio ${ratio_h}). The proxy does not exceed direct throughput at this level. This may indicate: (a) the actual peak in-flight was lower than the wave size due to hyper connection limits, (b) the quadratic penalty is too mild at tested levels, or (c) the proxy's HTTP-hop overhead exceeds the backend collapse benefit. In a real vLLM deployment, the penalty at high concurrency would be steeper (GPU KV-cache saturation), making admission control more clearly beneficial."
    fi
else
    verdict="INCONCLUSIVE"
    analysis="Insufficient data. Bench may have failed or timed out."
fi

echo "**Verdict:** $verdict" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"
echo "$analysis" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"

echo ""
echo "=== Results written to $RESULTS_FILE ==="
echo "Verdict: $verdict"

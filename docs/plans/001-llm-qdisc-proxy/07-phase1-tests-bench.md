# 07 — Phase 1 Integration Tests + Benchmark Harness

**Phase:** 1 (Basic Queue Proxy) — **PHASE GATE**
**Depends on:** `03`, `04`, `05`, `06`.

## Objective

Close Phase 1 with (a) end-to-end integration tests covering the full
gateway + queue + backpressure + metrics surface, and (b) a reproducible
benchmark that proves the Phase 1 success criteria: **higher aggregate
tokens/sec than uncontrolled concurrency** (PRD §G1, §14).  Phase 1 is
"done" only when this issue passes.

## Files

| File | Change |
| --- | --- |
| `tests/phase1_e2e.rs` | New: full-stack tests against a stub vLLM with configurable latency. |
| `benches/throughput.rs` | New: criterion bench — proxy vs direct, bursty load. |
| `benches/stub_backend.rs` | New: shared stub that emits N fake tokens with timing. |
| `scripts/phase1_bench.sh` | New: reproducible bench runner + ascii reporter. |
| `docs/plans/001-llm-qdisc-proxy/PHASE1-RESULTS.md` | New: where recorded numbers land. |

## Steps

1. Build a stub backend (`hyper` server) that:
   * Accepts `/v1/chat/completions` (stream + non-stream),
   * Sleeps a per-request cost then emits a configurable number of fake SSE
     `data:` frames plus a final `usage` frame,
   * Tracks concurrent in-flight count for assertions.
2. End-to-end tests:
   * burst of 50 reqs with `max_active_flows=4` shows backend never sees >4
     concurrent,
   * streaming ordering preserved across queued clients,
   * 429 path returns `Retry-After` and a later retry succeeds,
   * `/metrics` reflects activity during the run.
3. Criterion bench comparing:
   * **direct**: clients -> stub,
   * **proxy**: clients -> llm-qdisc-proxy -> stub,
   across concurrency levels `{1,4,8,16,32}` and burst lengths.
   Report aggregate fake-tokens/sec.
4. `scripts/phase1_bench.sh`: sets env, runs `cargo bench`, parses output,
   writes the comparison table to `PHASE1-RESULTS.md`.
5. Run the bench; record numbers.  Confirm **proxy tokens/sec >= direct
   tokens/sec at high concurrency** (the central Phase 1 thesis: a lightly
   loaded GPU doesn't need the proxy, but a bursty overloaded one does).
   If the result does **not** hold, document the gap and open a follow-up
   instead of closing this issue.

## Verification

* `cargo test --test phase1_e2e` green.
* `cargo bench` runs cleanly; `PHASE1-RESULTS.md` contains the comparison
  table with the recorded numbers.
* The Phase 1 success criterion from `PLAN.md` — "aggregate tokens/sec vs
  uncontrolled concurrency" — is explicitly judged PASS or GAP in
  `PHASE1-RESULTS.md`.
* `scripts/phase1_bench.sh` is re-runnable from a clean checkout.

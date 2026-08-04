# 17 — Phase 3 Integration Tests Against Live vLLM

**Phase:** 3 (vLLM Integration) — **PHASE GATE**
**Depends on:** `15`, `16`.
**Blocks:** (none — closes the plan; archive follow-up is the plan-process completion step).

## Objective

Close Phase 3 with tests that exercise the full proxy against a **real vLLM
backend**, gated behind an env flag so GPU-less CI stays green.  Phase 3 is
"done" when this issue passes on a machine that has vLLM running.

Validate the PRD §14 success metrics end-to-end:

| Metric | Target |
| --- | --- |
| Aggregate throughput | +20% vs uncontrolled concurrency |
| GPU utilization variance | reduced |
| OOM failures | near zero |
| Agent completion latency | improved |
| Starvation events | zero |
| Queue visibility | complete |

## Files

| File | Change |
| --- | --- |
| `tests/phase3_live.rs` | New: live vLLM integration; `#[ignore]` unless `TINYLLB_LIVE_TESTS=1`. |
| `scripts/phase3_bench.sh` | New: harness for the +20% throughput comparison. |
| `scripts/spawn_vllm.sh` | New: helper to launch a small vLLM server for tests. |
| `docs/plans/001-tinyllb/PHASE3-RESULTS.md` | New: where the §14 metric table gets filled in. |

## Steps

1. Live test harness bootstraps vLLM via `scripts/spawn_vllm.sh` (model
   path + args from env; document a tiny model like
   `meta-llama/Llama-3.2-1B-Instruct` so it fits a single modest GPU).  If
   env unset, the suite is skipped (`#[ignore]`).
2. Tests (`tests/phase3_live.rs`, gated by `TINYLLB_LIVE_TESTS=1`):
   * **Throughput delta**: identical 50-request bursty workload, run twice:
     direct-to-vLLM, then via-proxy with `algorithm=drr`, compare aggregate
     tokens/sec; assert via-proxy >= 1.2 * direct (PRD §14 +20% target).
   * **OOM-free**: run a deliberately oversized concurrent burst (e.g. 64
     requests each `max_tokens=4096` on a small model); assert zero 5xx
     due to KV-cache failure (retry not counted as success; assert no failures
     either).
   * **No starvation**: run 10 distinct flows with wildly different weights
     + completion_bias ON; assert no flow unanswered after
     `starvation_timeout`.
   * **Queue visibility**: `GET /queue` reflects reality during the run.
3. `scripts/phase3_bench.sh`: orchestrates spawn_vllm, runs live tests +
   `14`'s fairness bench against the live backend, parses results into
   `PHASE3-RESULTS.md` filling in PRD §14's table.
4. Judge PASS / GAP for each §14 metric in `PHASE3-RESULTS.md`.  If a metric
   fails, the issue does NOT close — open a follow-up issue in the next plan
   and keep this one open.

## Verification

* `cargo test --all` (without `TINYLLB_LIVE_TESTS`) green, the live suite
  is skipped.
* `TINYLLB_LIVE_TESTS=1 scripts/phase3_bench.sh` against a real vLLM fills
  in `PHASE3-RESULTS.md`.
* The §14 table has a PASS/GAP verdict per row; **all six rows PASS** is the
  condition to close this issue and Phase 3.
* On a GPU-less machine: `scripts/phase3_bench.sh` prints a clear "set
  TINYLLB_LIVE_TESTS=1 + provide a vLLM endpoint" message and exits non-zero
  without running the comparison.

# 14 — Phase 2 Integration Tests + Fairness / Throughput Benchmarks

**Phase:** 2 (Agent Scheduling) — **PHASE GATE**
**Depends on:** `08`, `09`, `10`, `11`, `12`, `13`.

## Objective

Close Phase 2: prove the agent-scheduling goals (PRD §G2, §G3) and the
"completion-bias improves outcomes" claim quantitatively.  Phase 2 is "done"
only when these tests + benchmarks pass.

## Files

| File | Change |
| --- | --- |
| `tests/phase2_e2e.rs` | New: end-to-end multi-flow scenarios. |
| `benches/fairness.rs` | New: per-flow throughput distribution test. |
| `benches/completion_bias.rs` | New: 10-agents-@-10% vs 3-agents-@-90% comparison. |
| `scripts/phase2_bench.sh` | New: runner; writes `PHASE2-RESULTS.md`. |
| `docs/plans/001-llm-qdisc-proxy/PHASE2-RESULTS.md` | New: recorded numbers + verdict. |

## Steps

1. End-to-end scenarios (`tests/phase2_e2e.rs`):
   * **Weighted fairness**: 3 flows weights `10/5/1`, identical workloads;
     assert throughput ratio within tolerance of weights across runs.
   * **No starvation**: 1 heavy (`background`) + 1 light (`interactive`);
     interactive never waits longer than `starvation_timeout`.
   * **Completion bias**: 10 distinct flows starting long requests
     simultaneously with `target_active_flows=3`: only 3 make progress past
     admission until those 3 finish, etc.
   * **`GET /queue` correctness**: mid-run assert `active`/`waiting`/
     `flows[].position`.
2. `benches/fairness.rs`: criterion — drive stub backend, measure per-flow
   completed-work units; emit a fairness score (max/min ratio).
3. `benches/completion_bias.rs`: demonstrate the "10 agents @ 10%" scenario:
   count **completed** flows within a fixed wall-clock budget when
   completion_bias is ON vs OFF.  The proxy should complete more flows when
   ON (PRD §6.6).
4. `scripts/phase2_bench.sh`: runs both benches, parses output, writes
   `PHASE2-RESULTS.md` with:
   * fairness ratio table,
   * completion_bias on/off completion counts,
   * explicit PASS / GAP verdict per goal.
5. Judge success explicitly:
   * **G2 (no starvation)**: no flow waits > `starvation_timeout`.
   * **G3 (agent-aware)**: per-flow metrics + `GET /queue` correct.
   * **Completion bias**: completion-bias ON completes > completion-bias OFF.

## Verification

* `cargo test --test phase2_e2e` green.
* `cargo bench` runs both bench files cleanly.
* `PHASE2-RESULTS.md` contains the comparison tables and a PASS/GAP verdict
  for each of G2, G3, and completion bias.
* Phase 1 `PHASE1-RESULTS.md` numbers do not regress (re-run `07`'s
  `scripts/phase1_bench.sh` and compare).

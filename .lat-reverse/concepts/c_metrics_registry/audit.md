# Audit: c_metrics_registry

**Auditor**: Final-cycle Auditor (LAT Reconstruction Pipeline)
**Spec**: `.lat-reverse/concepts/c_metrics_registry/spec.md` (twice-corrected)
**Source**: `src/metrics/mod.rs`, `src/metrics/endpoint.rs`
**Date**: 2026-08-03

---

## Verdict: PASS (minor spec_error)

The spec and implementation are closely aligned. One spec_error found; no bugs, undocumented_behavior, or missing_interface gaps.

---

## "No How" Lint

**PASS** — No violations detected.

| Check | Result |
|---|---|
| Control flow descriptions | None found |
| Data structure details | None found |
| Function/method names as concept identifiers | None found (spec uses "constructor", "factory", "default-constructed value" — domain descriptions, not method names) |
| Implementation-specific terminology | "Prometheus metric name", "Prometheus text format", "Prometheus ecosystem" are domain context, not implementation leakage |

---

## Findings

### spec_error: Ambiguous per-flow scope in Operational Scope (§Purpose)

**Severity**: Low (resolved by later section)

The operational scope bullet reads:

> Collects **per-flow queue depth, wait-time distribution, and active-flow counts** for scheduling observability.

The modifier "per-flow" grammatically applies to all three items in the enumeration. However, the implementation provides:

- `queue_depth: GaugeVec` — per-flow (labeled by `flow_id`) ✓
- `queue_wait_seconds: Histogram` — **not** per-flow (no labels) ✗
- `active_flows: Gauge` — **not** per-flow (single scalar) ✗

The `## Metric families` section correctly resolves this — it states "per-flow queue depth (labeled by flow identity)", "wait-time histogram (buckets 0.01–30.0 seconds)" (no per-flow claim), and "count of active flows" (no per-flow claim). The later section is accurate; the earlier operational scope bullet is ambiguous and could mislead a reader who stops reading there.

**Recommendation**: Rephrase to "Collects per-flow queue depth, an aggregate wait-time distribution, and an active-flow count for scheduling observability."

---

## Verification Table

| Spec Claim | Implementation | Status |
|---|---|---|
| **Purpose** — Central collection point for operational telemetry | `Metrics` struct holds all collectors | ✅ Match |
| **Purpose** — Scrapeable surface via shared metrics value | `pub registry: Registry` exposed; `Arc<Metrics>` for sharing | ✅ Match |
| **Purpose** — Per-flow queue depth, wait-time distribution, active-flow counts | `queue_depth: GaugeVec`, `queue_wait_seconds: Histogram`, `active_flows: Gauge` | ✅ Match |
| **Purpose** — Cumulative token count, tokens-per-second rate | `tokens_generated_total: Counter`, `tokens_per_second: Gauge` | ✅ Match |
| **Purpose** — Active-request depth, server-error count (5xx/network only) | `requests_active: Gauge`, `errors_total: IntCounter` (help text: "5xx responses and network errors") | ✅ Match |
| **Purpose** — Backpressure rejections by mode | `backpressure_rejections_total: CounterVec` (labeled `["mode"]`) | ✅ Match |
| **Purpose** — Per-flow scheduling credit | `flow_credit: GaugeVec` (labeled `["flow_id"]`) | ✅ Match |
| **Purpose** — Per-flow starvation-wait, force-admit count | `flow_starvation_seconds: GaugeVec` (labeled `["flow_id"]`), `starvation_force_admits_total: IntCounter` | ✅ Match |
| **Purpose** — Request lifecycle events (4 types) | `request_events_total: CounterVec` (labeled `["event"]`, help text lists all four) | ✅ Match |
| **Purpose** — KV-cache usage %, free %, admission decisions | `vllm_kv_cache_usage: Gauge`, `vllm_kv_cache_free: Gauge`, `kv_admission_decisions_total: CounterVec` (labeled `["decision"]`) | ✅ Match |
| **Interface** — Zero-argument constructor | `pub fn new() -> Self` | ✅ Match |
| **Interface** — Default equivalent to zero-argument | `impl Default { default() → Self::new() }` | ✅ Match |
| **Interface** — Shareable handle factory | `pub fn create_metrics() -> Arc<Metrics>` | ✅ Match |
| **Interface** — Public submodules: backend, endpoint, queue, throughput | Lines 1-4: all four declared `pub mod` | ✅ Match |
| **Interface** — Queue wait-time histogram buckets 0.01–30.0s | Buckets: `[0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0]` — range accurate | ✅ Match |
| **Interface** — Ephemeral flows labeled `"ephemeral"` | Doc comment: "Ephemeral flows aggregate to the label value `"ephemeral"`" | ✅ Match |
| **Interface** — Exposition: `GET /metrics`, `200 OK`, `text/plain; version=0.0.4` | `endpoint.rs` sets content type header, returns 200 by default | ✅ Match |
| **Interface** — Encoding failure → `500` with error log | `endpoint.rs` returns `INTERNAL_SERVER_ERROR.into_response()` + `tracing::error!` | ✅ Match |
| **Invariant** — Registration completeness (exposed = registered) | All 16 collectors created and registered via `registry.register()` | ✅ Match |
| **Invariant** — Name stability (distinct, fixed names) | All 16 metric names are string literals; no duplicates | ✅ Match |
| **Invariant** — Single registry | Single `Registry` instance; all collectors registered there | ✅ Match |
| **Invariant** — Concurrent observability via shared handles | `Arc<Metrics>` enables shared access; prometheus collectors are thread-safe | ✅ Match |
| **Constraint** — Infallible construction (panic on failure) | All 32 `.expect()` calls (16 creation + 16 registration) | ✅ Match |
| **Constraint** — Fixed metric set | All collectors created in `new()`, no runtime extension API | ✅ Match |
| **Constraint** — Prometheus binding | Uses `prometheus` crate types throughout | ✅ Match |
| **Constraint** — Single-instance assumption | `create_metrics()` returns a new `Arc<Metrics>`; no enforcement, matches "assumes" language | ✅ Match |
| **Non-goals** — No logging, alerting, data transformation | Correct — only metric collection and exposition | ✅ Match |
| **Non-goals** — Not configurable at runtime | Correct — metric set fixed at construction | ✅ Match |
| **Non-goals** — Not general-purpose metrics SDK | Correct — domain-specific inventory | ✅ Match |

---

## Summary

- **Findings**: 1 (spec_error, low severity)
- **Spec accuracy**: 26/26 claims verified against implementation (1 minor ambiguity in Purpose that is correctly resolved in Interface)
- **"No How" compliance**: Pass
- **Net assessment**: The spec accurately describes the implementation. The single finding is a scoping ambiguity in the Purpose section that does not propagate — the Interface/Metric families section is precise and correct.

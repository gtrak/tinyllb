# Audit: Metrics Families (c_metrics_families)

**Auditor:** Final-cycle Auditor
**Status:** DRAFT
**Artifacts compared:** `spec.md` (twice-corrected) vs. `src/metrics/mod.rs`, `src/metrics/backend.rs`, `src/metrics/queue.rs`, `src/metrics/throughput.rs`, `src/metrics/endpoint.rs`

---

## "No How" Lint

**Result: PASS**

The spec avoids control flow descriptions, data structure internals, and implementation-specific terminology throughout. The only borderline statement is:

- *Constraints:* "If collector creation fails during initialization, construction aborts fatally with no runtime recovery." — The word "initialization" describes a phase, but the clause is purely behavioral ("construction aborts fatally"). Acceptable.

---

## Findings

### 1. Undocumented Behavior — `Metrics.registry` Field

**Severity:** Low

The `Metrics` struct exposes `pub registry: Registry` as a public field. This provides direct access to the Prometheus `Registry` for custom collector registration or gathering, beyond what any documented construction or field-access surface describes. Callers can call `registry.gather()`, `registry.register()`, or unregister collectors — none of which are documented.

**Location:** `src/metrics/mod.rs:16-17`

**Classification:** `undocumented_behavior`

---

### 2. Undocumented Behavior — Labeled Metrics on `llm_queue_depth` (Cleanup Semantics)

**Severity:** Medium

The spec states `llm_queue_depth` is labeled by `flow_id` and that ephemeral flows aggregate to `"ephemeral"`. The implementation confirms the `GaugeVec` carries the `flow_id` label. However, no mechanism for removing or cleaning up stale `flow_id` labels after a flow terminates is documented. Prometheus `GaugeVec` labels persist across scrapes unless explicitly removed, which means completed flows leave dead label cardinality in the scrape output.

**Location:** `src/metrics/mod.rs:72-79`, spec line 50

**Classification:** `undocumented_behavior`

---

### 3. Missing Interface — Throughput Background Task

**Severity:** Medium

The spec states `llm_tokens_per_second` is "derived from the cumulative counter at regular intervals" and describes the background refresh as a constraint (stale without indication). The source in `src/metrics/throughput.rs:7-8` mentions "a background task that computes the rate from the counter" and "updated every second". However, the actual background task implementation (the `tokio::spawn` or equivalent), its spawn point, lifecycle management, shutdown behavior, and how it accesses the `Metrics` struct are entirely absent from the audited source files. This is the mechanism's contract — who owns it, when it starts, and what happens on shutdown.

**Location:** `src/metrics/throughput.rs:7-8`, spec lines 57, 76

**Classification:** `missing_interface`

---

### 4. Undocumented Behavior — `errors_total` Type (`IntCounter` vs `Counter`)

**Severity:** Low

The implementation uses `prometheus::IntCounter` for `errors_total` rather than `prometheus::Counter`. `IntCounter` uses `u64` internally and provides `get()` returning `u64`, while `Counter` uses `f64` and may expose different precision semantics for large values. The spec describes the metric contractually ("monotonically increasing counter of backend failures") but does not document the numeric type or its range/precision implications. For very long-running systems with high error rates, `u64` overflow differs from `f64` rounding.

**Location:** `src/metrics/mod.rs:32`, `src/metrics/mod.rs:112-116`

**Classification:** `undocumented_behavior`

---

### 5. Spec Error — Invariant vs Constraint on Gauge Negative Values

**Severity:** Medium

The spec contains a tension between two statements:

- *Invariants:* "`vllm_requests_active` equals dispatched requests minus completed requests; its value reflects in-flight backend requests." — This claims an equality invariant that assumes the value is always non-negative (you cannot have "negative in-flight requests").
- *Constraints:* "Gauge values can become negative if decrement operations outnumber increments (e.g., double-completion), silently corrupting the measurement." — This admits the invariant can be violated.

The invariant is stated as unconditional ("the following statements hold regardless of implementation details"), yet the constraint explicitly describes how it can be violated. The invariant should either be qualified ("holds when callers correctly pair increments and decrements") or the constraint should be elevated to a stronger guarantee.

**Location:** spec lines 63, 74

**Classification:** `spec_error`

---

### 6. Undocumented Behavior — No Error Surface for Metric Registration

**Severity:** Low

All collectors are registered with `.expect("...should succeed")` (e.g., `src/metrics/mod.rs:79`). If registration fails (e.g., duplicate registration in the same registry), `Metrics::new()` panics. The spec describes this as "construction aborts fatally" in Constraints, which is accurate, but the actual panic behavior (unrecoverable, no error returned to the caller) is an implementation detail not surfaced in the Interface section. The spec's Interface section presents `Metrics::new()` as a straightforward constructor without documenting that it can panic.

**Location:** `src/metrics/mod.rs:79-226`, spec lines 32

**Classification:** `undocumented_behavior`

---

### 7. Undocumented Behavior — `llm_queue_wait_seconds` Bucket Configuration

**Severity:** Low

The implementation uses 9 explicit buckets: `[0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0]`. The spec's Non-goals state "Histogram bucket configuration is not a contract for consumers; bucket boundaries may shift without breaking callers." However, the specific bucket values chosen (10 ms granularity at the low end, multiplicative spacing through the range) are observable behavioral details. The current scrape output is determined by these exact buckets, which is a real behavioral property of the interface even if not a stability guarantee.

**Location:** `src/metrics/mod.rs:81-88`, spec lines 21

**Classification:** `undocumented_behavior`

---

### 8. Missing Interface — No `metrics_handler` Route Registration Documented

**Severity:** Low

The spec describes `GET /metrics` as a scrape endpoint but does not document where or how this route is registered (router module, mount point, middleware dependencies). The `metrics_handler` function in `endpoint.rs` uses `State<AppState>` and accesses `state.metrics.registry.gather()`, but the route binding (which Axum router, at which path) is outside the audited source files.

**Location:** `src/metrics/endpoint.rs:12`, spec line 37

**Classification:** `missing_interface`

---

### 9. Undocumented Behavior — CounterVec Label Keys on Additional Families

**Severity:** Low

Four additional families use `CounterVec` or `GaugeVec` with specific label keys that are not described in the spec:

| Metric | Label Key |
|---|---|
| `llm_backpressure_rejections_total` | `mode` |
| `llm_flow_credit` | `flow_id` |
| `llm_flow_starvation_seconds` | `flow_id` |
| `llm_request_events_total` | `event` |
| `llm_kv_admission_decisions_total` | `decision` |

The spec acknowledges these families exist ("Additional metric families beyond the three primary groups reside in the same registry") but does not document their label dimensions, valid label values, or cardinality constraints.

**Location:** `src/metrics/mod.rs:35-57`, spec line 13

**Classification:** `undocumented_behavior`

---

### 10. No Finding — Scrape Endpoint Contract (Verified)

The `GET /metrics` endpoint in `endpoint.rs` matches the spec:

- Returns Prometheus text format via `TextEncoder`
- Content-Type header: `text/plain; version=0.0.4`
- Encoding errors return `500` (via `INTERNAL_SERVER_ERROR`)
- Successful responses return default `200 OK` (axum default)

**Classification:** Verified match

---

### 11. No Finding — Construction Functions (Verified)

All three construction surfaces match:

- `create_metrics()` returns `Arc<Metrics>` (source line 250-252)
- `Metrics::new()` creates standalone instance with all collectors registered (source lines 69-246)
- `Metrics::default()` delegates to `Metrics::new()` (source lines 60-64)

**Classification:** Verified match

---

### 12. No Finding — Field Names Match (Verified)

Every primary family collector is a public struct field matching the spec's field-access surface:

| Spec Field | Source Field | Metric Name |
|---|---|---|
| `requests_active` | `requests_active` | `vllm_requests_active` |
| `errors_total` | `errors_total` | `vllm_errors_total` |
| `queue_depth` | `queue_depth` | `llm_queue_depth` |
| `queue_wait_seconds` | `queue_wait_seconds` | `llm_queue_wait_seconds` |
| `active_flows` | `active_flows` | `llm_active_flows` |
| `tokens_generated_total` | `tokens_generated_total` | `llm_tokens_generated_total` |
| `tokens_per_second` | `tokens_per_second` | `llm_tokens_per_second` |

**Classification:** Verified match

---

## Summary

| Classification | Count | Severity |
|---|---|---|
| `spec_error` | 1 | Medium |
| `missing_interface` | 2 | Medium / Low |
| `undocumented_behavior` | 6 | Low (4), Medium (2) |
| Verified match | 3 | — |

**Primary concerns:**

1. **Spec Error (#5):** The unconditional invariant on `vllm_requests_active` contradicts the explicit constraint that gauge values can go negative. Either qualify the invariant or acknowledge the conditional nature.
2. **Missing Interface (#3):** The throughput background task that drives `llm_tokens_per_second` has no documented spawn point, lifecycle, or shutdown contract in the audited scope.
3. **Missing Interface (#8):** The scrape endpoint's route registration (which router, what middleware) is not documented.

**Secondary concerns:**

4. The `Metrics.registry` field is publicly exposed but not documented in the spec's interface.
5. Stale label cleanup semantics for `flow_id`-labeled metrics are not specified.
6. Additional metric families (backpressure, DRR, starvation, lifecycle, KV cache) are acknowledged but their label keys and contracts are not documented.

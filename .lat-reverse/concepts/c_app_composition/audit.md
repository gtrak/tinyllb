# App Composition — Audit Report

**Scope:** `.lat-reverse/concepts/c_app_composition/spec.md` vs `src/main.rs`
**Date:** 2026-08-03
**Auditor:** Cycle 2 — fresh audit (previous audit ignored)

---

## Verdict: PASS with minor findings

The spec is materially consistent with the implementation. No bugs, no spec errors, no missing interfaces. Two minor undocumented-behavior notes. "No How" lint passes.

---

## Interface: spec vs implementation

| # | Spec claim | Implementation evidence | Status |
|---|-----------|------------------------|--------|
| 1 | Router construction accepts fully wired application state and returns a composed HTTP router | `pub fn create_router(state: gateway::AppState) -> Router` (line 13) | ✅ Consistent |
| 2 | Port resolution gated precedence: `LLM_QDISC__SERVER__BIND` set → configured bind used unconditionally; else `PORT` parsed as `0.0.0.0:<port>`; else configured bind | Lines 67–76: `is_ok()` on env var → `cfg.server.bind`; else `PORT` parsed → `0.0.0.0:{port}`; else `cfg.server.bind` | ✅ Consistent |
| 3 | Health endpoint responds HTTP 200 with body `"ok"` regardless of state | `healthz() -> &'static str "ok"`, test asserts 200 + `"ok"` (lines 9, 182–187) | ✅ Consistent |
| 4 | Application state shared read-only across sub-routers except health | Health router built without `.with_state()`; metrics/gateway/admin all receive `state` or `state.clone()` (lines 14–19) | ✅ Consistent |
| 5 | Telemetry initialized before configuration loading; first observable startup step | `telemetry::init()` (line 62) precedes `config::load()` (line 64) | ✅ Consistent |

---

## Invariants: spec vs implementation

| # | Spec claim | Implementation evidence | Status |
|---|-----------|------------------------|--------|
| 1 | Exactly four sub-routers: health, metrics, gateway, admin | Lines 14–19: four routers created; lines 21–25: exactly four `.merge()` calls | ✅ Consistent |
| 2 | Health always reachable and always responds successfully, independent of all application state | `healthz()` is a stateless `&'static str`; no error path; no `.with_state()` on health router | ✅ Consistent |
| 3 | All non-health sub-routers share the same application state instance | `state.clone()` for metrics (17), gateway (18); `state` moved to admin (19); all share `gateway::AppState` | ✅ Consistent |
| 4 | Token-rate background task runs for lifetime of process; backend monitor runs only when KV policy enabled | `spawn_token_rate_task()` (line 131) — infinite loop via `loop {}`; monitor spawned inside `if cfg.kv_policy.enabled` (lines 87–101) | ✅ Consistent |
| 5 | Configuration loaded before any service construction or network binding | `config::load()` (line 64) before all construction (lines 78–127) and bind (line 135) | ✅ Consistent |

---

## Constraints: spec vs implementation

| # | Spec claim | Implementation evidence | Status |
|---|-----------|------------------------|--------|
| 1 | Single-process model: no fork or multiplexing | Single `main()` entry, no `fork`, no process spawning beyond Tokio tasks | ✅ Consistent |
| 2 | No graceful shutdown; server runs until fatal error terminates process | `listener.unwrap()` (line 137), `axum::serve(...).await.unwrap()` (line 138) — no signal handling, no shutdown hook | ✅ Consistent |
| 3 | All initialization failures are fatal; partial startup not possible | `.expect("failed to load configuration")` (line 64), `.unwrap()` on bind and serve — no partial path | ✅ Consistent |
| 4 | Rolling average window minimum floor of one second | `window_secs.max(1)` (line 35) | ✅ Consistent |
| 5 | Malformed `PORT` values terminate the process | `port_str.parse().expect("PORT must be a valid port number")` (line 70) | ✅ Consistent |

---

## Undocumented Behavior (2 findings)

### UNDOC-1: `spawn_token_rate_task` is a public function not surfaced in Interface

`pub fn spawn_token_rate_task(metrics: &Arc<Metrics>, window_secs: u64)` (line 32) is publicly exported but not listed as an interface surface in the Interface section. It is referenced in Invariants ("the token-rate background task runs for the lifetime of the process") but the callable contract — parameters, preconditions, postconditions — is absent from the Interface.

- **Class:** `undocumented_behavior`
- **Severity:** low — internal helper exposed as `pub` for testability or future extensibility; no external consumer currently depends on it.
- **Recommendation:** Either document `spawn_token_rate_task` as an interface surface or change visibility to `pub(crate)` / private.

### UNDOC-2: `create_router` precondition on state is implicit

The spec states "Router construction accepts fully wired application state" but does not enumerate what "fully wired" requires. The implementation accepts `gateway::AppState` with all fields populated (client, backend_url, metrics, scheduler, flow_registry, backpressure, request_timeout). The spec does not define what happens if a field is uninitialized.

- **Class:** `undocumented_behavior`
- **Severity:** low — `AppState` construction is fully contained within `main()`, so the precondition is naturally satisfied.
- **Recommendation:** No action required unless `create_router` is intended as a reusable interface for external callers.

---

## Missing Interface: none

All observable public surfaces are either documented or internal-only. No gap found.

---

## "No How" Lint

The spec is checked against the "No How" constraint:

| Check | Result |
|-------|--------|
| Control flow descriptions | ❌ None found. The spec describes *what* (contracts, invariants), not *how*. |
| Data structure details | ❌ None found. Field lists and internal types are absent. |
| Function/method names as concept identifiers | ❌ None found. Concepts are named by domain role (health endpoint, port resolution), not by function name. |
| Implementation-specific terminology | ❌ None found. Language is domain-level (router, endpoint, telemetry, configuration). |

**"No How" lint: PASS**

---

## Summary

| Category | Count |
|----------|-------|
| Bug | 0 |
| Spec error | 0 |
| Undocumented behavior | 2 |
| Missing interface | 0 |

The spec is accurate and complete for its stated scope. The two undocumented-behavior items are low-severity observations about public visibility of an internal helper function. No contradictions found between spec and implementation.

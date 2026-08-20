# 01 — Interactive sessions starve under KV pressure; non-inference requests queue

Status: **RESOLVED & DEPLOYED (2026-08-20)** · Component: `tinyllb` (scheduler admission, gateway routing)
Reported: 2026-08-20 · Severity: high (interactive UX broken under concurrent agent load)

## Resolution Summary

Both defects are fixed, reviewed, committed, and deployed to the live
tinyllb service on 2026-08-20.

### What changed

**Change 1 — priority-100 flows bypass the KV gate.** `KvPolicy::check` now
takes `is_interactive: bool` and, when `kv_policy.bypass_interactive` is enabled
(default `true`), records a `bypass` decision and returns `Ok(())` immediately,
skipping both the delay band and the reject threshold. `Scheduler` stores
`interactive_priority` (= `priorities.interactive`) and threads `is_interactive`
at the single `check` call site. Interactive sessions never see a KV-gate 429;
the DRR scheduler + `max_active_flows` still bound concurrency and vLLM's
preemption handles true KV exhaustion.

**Change 2 — only inference requests queue.** `proxy_handler` now branches on
`is_inference_request` (POST `/v1/chat/completions` or `/v1/completions` only).
Non-inference requests (`GET /v1/models`, health probes, unknown routes) take a
lean passthrough that shares only the 32 MiB body guard, header filtering, and
backend URL building, then forwards the backend response verbatim with
`X-Request-ID` echoed. They never touch the scheduler, lifecycle, ticket, retry,
token accounting, or the flow/cadence registry, so metadata is never held behind
inference backpressure or a KV-gate 429. Network error still maps to 502 (a dead
backend yields 502, preserving `test_backend_unreachable_returns_502`). The
inference path is byte-for-byte unchanged (pure-additive diff).

### Config knob
`kv_policy.bypass_interactive: true` (serde default; loader default). Documented
in `lat.md/admission.md` (an Interactive Bypass invariant) and `config.example.yaml`.

### Tests
- `tests/kv_admission.rs`: +4 tests — interactive flows bypass delay (0.85) and
  reject (0.96) with `bypass` metric == 1; background flows pinned via
  `apply_priority_override` still delay (`delay`==1) and reject (`reject`==1)
  with `bypass`==0. Existing tests pinned to `bypass_interactive: false` so they
  exercise the gate unchanged. 13/13 green.
- `tests/gateway.rs`: +1 test `test_models_passthrough_under_kv_pressure` — under
  KV 0.96 a background inference POST is 429'd while `GET /v1/models` returns 200
  byte-identical. Extracted `build_app_from_state` (consolidates router wiring).
  11/11 green.

### Commits
- `c4e6ffa` feat(scheduler): bypass KV admission gate for interactive flows
- `dbaa141` feat(gateway): bypass admission gate for non-inference requests
- `8415f6c` test: cover KV interactive bypass and metadata scheduler bypass
- `9875873` docs: document kv_policy.bypass_interactive invariant and knob

### Deployment (2026-08-20)
`cargo build --release` → `~/.local/bin/tinyllb`; added `bypass_interactive: true`
to the live `~/.config/tinyllb/config.yaml`; `systemctl --user restart tinyllb`.
Verified: config loaded with `bypass_interactive: true`; the `bypass` metric
counted up (`llm_kv_admission_decisions_total{decision="bypass"}` > 0) under
priority-100 traffic; `GET /v1/models` → 200.

---

*Original report below.*



## Summary

Two admission-gate defects surface together when many concurrent agent flows are
running and a new interactive session is started:

1. **The KV-cache admission gate ignores priority.** A confirmed-interactive
   (priority-100) flow is delayed/rejected by `kv_policy.check()` exactly like a
   background flow. Under sustained agent load the KV cache sits in the delay band
   (>0.80) indefinitely, so the interactive request parks at the gate forever and
   never reaches the DRR scheduler — the user sees nothing come back.

2. **Only inference should queue; metadata requests do not.** `GET /v1/models`
   (and any future non-POST route) is routed through `proxy_handler`, which
   unconditionally calls `admit_with_turn_boundary`. A metadata lookup can be held
   behind inference backpressure, which both hurts clients and makes observability
   (curl probes) unreliable.

Both are "the admission gate is too broad — it gates things that should not be
gated." This report covers the live evidence, the root cause in the source, a
proposed fix, test impact, and deployment steps.

## Decisions taken (during investigation)

- **Bypass scope:** *All priority-100 flows* bypass the KV gate — including
  optimistic `Cold` flows, not only `Interactive`. Rationale: the `Cold`/`Interactive`
  cadence philosophy already treats brand-new flows as interactive (priority 100)
  until evidence says otherwise; the KV gate should not contradict that.
- **Bypass depth:** Bypass **both** the delay band (>0.80) **and** the reject
  threshold (>0.95 → 429). The DRR scheduler + `max_active_flows=4` still bounds
  concurrency, and vLLM's own preemption handles true KV exhaustion. An interactive
  session must never see a KV-gate 429.
- **Config knob:** Add `kv_policy.bypass_interactive: bool` (default `true`) so the
  behavior is ops-toggleable and so the existing kv_admission tests can exercise the
  delay/reject paths unchanged by flipping it off.

## Evidence (live system, 2026-08-20 ~14:00 UTC)

vLLM serving on 2x RTX 5060 Ti (`max-num-seqs 4`, `max-active-flows 4` in tinyllb).
Eight concurrent flows; seven are agentic (`AgenticConfirmed`, priority 10) and one
is the user's new interactive session.

From `curl localhost:1234/metrics`:

```
llm_active_flows 4
llm_flow_cadence_state{flow_id="ses_fe1319610ffe5JA1tRSho3w3Oj"} 1   # Interactive
llm_flow_priority_class{flow_id="ses_fe1319610ffe5JA1tRSho3w3Oj"} 100
llm_queue_depth{flow_id="ses_fe1319610ffe5JA1tRSho3w3Oj"} 0
```

From `journalctl --user -u tinyllb` over the previous 2h, flow
`ses_fe1319610ffe5JA1tRSho3w3Oj` (the interactive session) has **zero** `admit
decision` log lines. Meanwhile agentic flows are being admitted but with ballooning
waits:

```
13:58:04  ses_fe098d18... admit accept wait_seconds=108.6  priority=10
13:58:04  ses_fe098a9c... admit accept wait_seconds=76.2   priority=10
14:00:05  ses_fe098746... admit accept wait_seconds=165.9  priority=10
14:00:44  ses_fe098f2f... admit accept wait_seconds=194.3  priority=10
```

Interpretation: the interactive request is stuck **before** the admit point (its
`queue_depth` is 0 and it never logs a decision), while agent waits grow to 3+
minutes. The KV gate (`delay_threshold: 0.80`, blocking mode, unbounded wait) is the
only thing that parks a request before the DRR admit log. A `backend inference
stall` + `ss -K`-style recovery fired at 13:56 (stall cleared), but the interactive
flow remained stuck afterward.

Separately, probing the backend's `/metrics` directly on `:8000` returned a
connection reset (a vLLM-side issue, out of scope here), and probing via the proxy
`:1234` path demonstrated that metadata routes share fate with inference under
backpressure.

## Root cause 1 — KV gate is priority-blind

Call chain (production):

```
proxy_handler                         src/gateway/proxy.rs:242
  └─ scheduler.admit_with_turn_boundary   src/scheduler/mod.rs:202
       ├─ cadence.record_arrival / classify_and_apply   (priority computed, line 218)
       ├─ self.kv_policy.check().await?                 src/scheduler/mod.rs:240
       │       └─ KvPolicy::check                         src/scheduler/kv_admission.rs:150
       │            decide(): delay if kv_usage>0.80, reject(429) if >0.95  (line 118)
       └─ self.inner.admit(flow_id, work_unit)          (DRR — never reached)
```

At `scheduler/mod.rs:240` the flow's priority (`flow.priority()`, computed at line
218 and stored on the `Arc<Flow>`) and its cadence state (line 229) are already
known, but `KvPolicy::check(&self)` takes **no** flow context:

```rust
// src/scheduler/kv_admission.rs:150
pub async fn check(&self) -> Result<(), BackpressureRejected> {
    if !self.enabled { return Ok(()); }
    let snapshot = match self.monitor.snapshot() { ... };
    match self.decide(&snapshot) {        // delay / reject / accept — no priority input
        KVMDecision::Delay  => { /* unbounded wait in blocking mode */ }
        KVMDecision::Reject => Err(BackpressureRejected { retry_after }),
        ...
    }
}
```

`flow` is an `Arc<Flow>` returned by `registry.get_or_create` (flow/mod.rs:228, no
DashMap guard held), so it is safe to read across the `.await`. The gate simply has
no idea the request is interactive.

`KvPolicy::check` has exactly **one** call site (`scheduler/mod.rs:240`), and the
non-turn-boundary `admit` wrapper is tests-only — so adding a parameter touches one
production site.

## Root cause 2 — metadata routes go through the scheduler

```rust
// src/gateway/mod.rs:75
pub fn create_router() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/completions", post(proxy_handler))
        .route("/v1/completions",      post(proxy_handler))
        .route("/v1/models",           get(proxy_handler))   // <-- metadata, but queued
}
```

`proxy_handler` unconditionally runs the admission gate:

```rust
// src/gateway/proxy.rs:359
let _ticket = match state.scheduler
    .admit_with_turn_boundary(flow_id_for_admit, work_unit, is_turn_boundary).await
{ ... };
```

So `GET /v1/models` participates in `max_active_flows`/backpressure and the KV gate.
The handler already has an `is_chat` notion (lines 489 and 570) used for the
premature-stop retry gate — there is no equivalent "is this an inference request at
all" short-circuit before the scheduler.

## Proposed fix

### Change 1 — priority-100 bypasses the KV gate

`src/scheduler/kv_admission.rs`
- Change signature: `pub async fn check(&self, is_interactive: bool) -> Result<(), BackpressureRejected>`.
- Add a `bypass_interactive: bool` field set from the new config (see below).
- Right after the `!self.enabled` short-circuit (line 152), add:
  ```rust
  if self.bypass_interactive && is_interactive {
      self.metrics.kv_admission_decisions_total
          .with_label_values(&["bypass"]).inc();
      return Ok(());
  }
  ```
  This skips both the delay band and the reject threshold.
- Existing `decide()` and the delay/reject bodies are unchanged.

`src/scheduler/mod.rs`
- Add field `interactive_priority: u32` to `Scheduler`.
- In `new()` (line 62), store `priorities.interactive` (the `Priorities` arg already
  exists at line 75). In `new_with_defaults()` (line 144), store
  `Priorities::default().interactive`.
- At line 240:
  ```rust
  let is_interactive = flow.priority() == self.interactive_priority;
  self.kv_policy.check(is_interactive).await?;
  ```

`src/config/mod.rs` + `src/config/loader.rs`
- Add `bypass_interactive: bool` to `KvPolicyConfig` with
  `default_bypass_interactive() -> bool { true }` and wire the serde default.
- Add a loader default (`kv_policy.bypass_interactive = true`) alongside the
  existing `reject_threshold`/`delay_threshold` defaults.
- Validation: boolean only (no range check). Update the existing
  `kv_policy.enabled`-style docs.

### Change 2 — only inference requests queue

`src/gateway/proxy.rs`
- Add near the existing `is_turn_boundary_request` helper:
  ```rust
  fn is_inference_request(method: &axum::http::Method, path: &str) -> bool {
      method == axum::http::Method::POST
          && (path == "/v1/chat/completions" || path == "/v1/completions")
  }
  ```
  Mirrors the `is_chat` gate already at lines 489/570. Future POST inference routes
  must opt in explicitly; all GETs and unknown routes bypass.
- In `proxy_handler`, after the shared setup (body collection, size guard, header
  filtering, `build_backend_url`, `forwarded_body`/`headers` built — roughly line
  323), branch:
  - If `!is_inference_request(&method, &original_path)`: a lean passthrough — send
    the reqwest request (with `state.request_timeout`), collect the body, return it
    with filtered headers + `X-Request-ID`. **Do not** call
    `admit_with_turn_boundary`, do not create a `LifecycleGuard`, do not take a
    ticket, do not run premature-stop retry, do not inject `include_usage`, do not
    credit tokens or touch the flow/cadence registry.
  - Otherwise: continue with the existing inference path unchanged.

Keep all current pre-branch setup (body size guard, header filtering, backend URL)
shared so metadata requests still get correct hop-by-hop stripping and the 32 MiB
guard. Only the admission/scheduler/lifecycle/retry/token-accounting machinery is
skipped.

## Safety / edge-case analysis

- **KV exhaustion under interactive bypass.** Bypassing reject means an interactive
  admit at 96%+ KV can cause vLLM to preempt other flows' KV blocks. This is
  acceptable: vLLM preemption is the backend's own pressure-relief, and an
  interactive user is the one flow we most want to serve. The DRR
  `max_active_flows=4` still caps concurrency, so this cannot flood the engine.
- **`Cold` flows get KV bypass.** A brand-new agentic flow is `Cold` (priority 100)
  for its first few requests until it demotes through `AgenticSuspected`. During
  that window it bypasses the KV gate. Worst case: ~`agentic_suspected_threshold`
  (default 5) requests per new agentic flow skip the gate. Acceptable — this
  matches the existing DRR philosophy that treats `Cold` as interactive.
- **No DashMap guard across `.await`.** `flow` is `Arc<Flow>` from `get_or_create`
  (flow/mod.rs:228); reading `flow.priority()` at line 240 is lock-free. Confirmed.
- **Metadata passthrough loses no observability that mattered.** `/v1/models` and
  health probes do not need token accounting or lifecycle events; they are not
  inference. The `X-Request-ID` echo is preserved.
- **Stall gate still applies.** The stall-reject (`scheduler/mod.rs:246`) sits
  after the KV check; it is not bypassed for metadata because metadata does not
  reach the scheduler at all — which is correct (metadata should not 429 on a
  stalled engine; it should just be proxied, matching vLLM's own behavior where
  `/v1/models` answers while the engine is stalled).

## Test impact

### `tests/kv_admission.rs` (must change)
The existing tests use fresh `FlowId`s which are `Cold` = priority 100. With the new
default `bypass_interactive: true` they would all bypass instead of
delay/reject. Two options:

- **Recommended:** set `bypass_interactive: false` in the `enabled_kv_policy()`
  helper so the existing delay/reject tests exercise the bypass-disabled path
  **unchanged**, and add **new** tests with `bypass_interactive: true`:
  - interactive flow admits instantly at KV 0.85 (no delay);
  - interactive flow admits at KV 0.96 (no 429);
  - background flow (pinned via `registry.apply_priority_override(..,
    Some(PriorityClass::Background), ..)`) still delays/rejects — proves the gate
    still bites non-interactive.
- `build_scheduler` / `build_scheduler_with_mode` must also return the
  `Arc<FlowRegistry>` so tests can pin flows (currently the registry is created
  inside the helper and dropped).

### `tests/gateway.rs` (no breakage; add coverage)
Existing `/v1/models` tests (`test_models_get_forwarded`,
`test_backend_unreachable_returns_502`, `test_query_string_preserved`) use no
backpressure and pass unchanged. Add a test where the scheduler is saturated (4
active slots + KV 0.96) and `GET /v1/models` still returns 200, proving the
passthrough.

### `tests/priority_live.rs`, `priority_heuristic.rs`, `scheduler_drr.rs` (unaffected)
They use empty monitors (KV 0.0 → accept regardless) or the non-turn-boundary
`admit` wrapper (default `is_turn_boundary=true`). No behavioral change.

### `tests/phase2_e2e.rs`, `phase3_live.rs` (verify)
`phase3_live` checks proxy `/v1/models` parity with direct backend — still holds
(passthrough returns the backend body byte-for-byte). Re-run to confirm.

## Docs to update

- `lat.md/admission.md` — "KV-Cache-Aware Admission Gate": add an invariant —
  *priority-100 (interactive) flows bypass both delay and reject when
  `bypass_interactive` is enabled*; document the new config knob and the `bypass`
  metric label.
- `~/.config/tinyllb/config.yaml` — add under `kv_policy:`:
  ```yaml
  bypass_interactive: true
  ```
- `~/opt/vllm/WORKLOG.md` — log the experiment/decision per AGENTS.md.

## Deployment

1. `cargo build --release` in `~/dev/vllm-frontend`; copy
   `target/release/tinyllb` → `~/.local/bin/tinyllb`.
2. Add `bypass_interactive: true` to the live config.
3. `systemctl --user restart tinyllb.service`.
4. Verify:
   - `curl -s localhost:1234/metrics | grep kv_admission_decisions` shows a `bypass`
     label counting up under agent load with an interactive session attached.
   - Drive KV high with agent load and confirm a priority-100 session admits
     instantly (no `wait_seconds` at the KV gate; the DRR admit still logs
     `priority=100`).
   - `GET /v1/models` returns 200 while the scheduler is saturated.

## Out of scope

- The vLLM-side `/metrics` connection reset on `:8000` (a separate backend issue;
  the stall-watchdog and `ss -K` recovery are the existing mitigation).
- Context-compression (`context_policy.enabled: false`) — unrelated.
- The stale `install-lb.sh` (see AGENTS.md key-files table) — regenerate separately
  if needed.
- Whether `Cold` flows *should* be priority-100 is a cadence-policy question, not an
  admission question; this fix respects the current policy.

# Plan 004 — Interactive-vs-Batch Priority Heuristic

## Why

The proxy already has three priority classes (`interactive: 100`,
`agent: 50`, `background: 10`) and the DRR scheduler respects them via
`priority::select_best` (highest priority wins among eligible flows).
But **every flow receives `default_priority: 50`** on creation
(`FlowRegistry::get_or_create` in `src/flow/mod.rs:174`), so the
priority machinery is inert in practice.

Under the observed contention burst (`WORKLOG.md` 2026-08-03
19:47–19:58), 10+ concurrent sessions competed round-robin for 4
active slots with no prioritization. Interactive sessions (a human
waiting on a single response) sat in the same DRR queue as long-running
batch subagent flows that fire requests back-to-back. Wait times
escalated to 260s+. vLLM itself was healthy the whole time — this was
purely a proxy scheduling problem.

The signal that distinguishes the two workloads is already visible to
the proxy: **inter-request timing per flow**.

- **Interactive** — user sends a request, waits for the response, then
  thinks/types before the next. Inter-request gaps are typically
  >= 5–30s.
- **Batch / subagent** — fires requests back-to-back with minimal
  gaps (< 2s) while it churns through a task. The proxy sees a
  sustained rapid-fire stream from one `flow_id`.

This plan makes the proxy **automatically classify** flows by request
cadence and demote rapid-fire flows to `background` priority so
interactive sessions keep their scheduling precedence. It also adds an
**explicit `X-LLM-Priority` header** so any client can override the
heuristic and pin a flow to a known class.

This plan does **not** change the backpressure mode (still `blocking`).
A future plan can flip to `hybrid` for 429 responses once we trust the
priority assignment.

## What

### Auto-classification by request cadence

A new `flow::cadence` module tracks per-flow inter-request timing
in-memory. Each flow keeps a rolling history of arrival timestamps and
exposes a `classify()` function returning one of the three priority
classes (`interactive` / `agent` / `background`). Classification runs
on every `admit()` and updates `Flow.priority` in place. The DRR
scheduler — already reading `flow.priority()` in
`priority::select_best` — picks up the new value on the next
selection.

The classification rule, applied per `admit()`:

| Observed cadence | Priority | Meaning |
|------------------|----------|---------|
| < 2s median gap, >= 3 recent requests | `background` (10) | Rapid-fire batch session |
| 2s–30s median gap | `agent` (50) default | Default workload |
| > 30s median gap, or first request | `interactive` (100) | Human-paced |

Parameters (gap thresholds, sample window) are configurable.
Classification is best-effort: it leaves current behavior untouched when
fewer than 3 samples exist, so cold-start flows default to
`default_priority` until the cadence is learned.

### Explicit `X-LLM-Priority` header override

A new request header pins a flow to a chosen priority class. The
header is read in `flow::identify::resolve` (alongside flow ID
resolution) and registered as an explicit override on the flow. While
the override is set, the cadence heuristic is skipped for that flow.

Supported values (case-insensitive):
- `interactive` → 100
- `agent` → 50
- `background` → 10
- `auto` → unset the override, resume heuristic

The header is stored **per-flow**, not per-request: once pinned, the
flow keeps that priority until either a later request re-pins it or an
admin clears it via the existing `POST /flows` API.

### Flow priority bookkeeping

`Flow` already has `priority: AtomicU32` (`src/flow/mod.rs:64`) with
`set_priority`/`priority()`. The `POST /flows` admin API already
upserts explicitly. This plan adds:

- A new optional `priority_override: AtomicU8` field on `Flow` storing
  whether the priority is heuristic-derived or explicitly pinned.
- A cadence state struct held outside `Flow` (in a separate
  `CadenceRegistry`) to keep the `Flow` struct lean and avoid
  reindexing all existing test constructors.

### Metrics

- New `llm_flow_priority_class` gauge labeled by `flow_id`, value = the
  current numeric priority (100/50/10). Lets operators see drift in
  real time.
- New `llm_flow_priority_source` counter labeled
  `source={heuristic,header,admin}`, so we can tell auto classification
  from explicit pins.
- New `llm_flow_inter_request_seconds` histogram labeled by `flow_id`
  for observability of the cadence signal itself.

### Config

A new top-level `priority_policy` section:

```yaml
priority_policy:
  enabled: true
  interactive_gap_min: 30s     # gaps >= this => interactive
  background_gap_max: 2s       # gaps <= this, >=3 samples => background
  sample_window: 20            # rolling window of arrival timestamps kept per flow
  min_samples: 3               # samples required before heuristic demotes
```

`enabled: false` disables the heuristic entirely (current behavior).
Explicit header overrides still work when disabled — they are a
client intent signal, not a heuristic.

### Scheduler integration

Cadence classification happens **synchronously inside `admit()`** before
the flow is handed to the scheduling algorithm. Because `admit()` is
already an async function on every scheduler and already calls
`registry.get_or_create`, the integration point is one new call per
admit:

```rust
// in DrrScheduler::admit_blocking (and fifo/wfq equivalents)
let flow = self.registry.get_or_create(flow_id.clone());
self.priority_policy.classify_and_apply(&flow, &flow_id); // NEW
self.completion_bias_gate.check(&flow).await;
// ... existing queue insertion ...
```

The classify step is O(window) work — bounded and cheap (a few hundred
nanoseconds over a VecDeque of 20 `Instant`s).

### Starvation safety

Existing starvation protection (`scheduler/starvation.rs`,
`starvation_timeout: 300s`) already force-admits any flow waiting too
long, regardless of priority. That invariant is preserved: a
`background`-classed flow that is perpetually preempted by interactive
flows still gets force-admitted at the 300s deadline. This is the
correct safety net — background does not mean starved, it means
"deprioritized when contested."

The plan does **not** add any new starvation mechanism. The existing
one is sufficient.

## Scope

| Area | Change |
|------|--------|
| `src/flow/cadence.rs` | NEW: `Cadence`, `CadenceRegistry`, `classify()` |
| `src/flow/identify.rs` | EDIT: read `X-LLM-Priority` header, surface via a new `ResolvedFlow { id, priority_override }` |
| `src/flow/mod.rs` | EDIT: `Flow` gains `priority_override: AtomicU8`; `FlowRegistry` gains `cadence: CadenceRegistry` accessor; default constructors updated |
| `src/config/mod.rs` | NEW `PriorityPolicy` struct + `priority_policy` field on `Config` |
| `src/config/loader.rs` | Wire new defaults into `load()` |
| `src/scheduler/mod.rs` | Pass `priority_policy` through to scheduler constructors; call `classify_and_apply` in `admit()` dispatch |
| `src/scheduler/drr.rs`, `fifo.rs`, `wfq.rs` | Add `priority_policy: Arc<PriorityPolicy>` field; call it in their `admit_*` paths |
| `src/gateway/proxy.rs` | Read priority override from resolve; pass into admit |
| `src/metrics/mod.rs` | Register 3 new collectors |
| `src/api/flows.rs` | Optionally extend `RegisterFlowRequest` with `priority_source` readback (no behavior change to admin pinning) |
| `tests/priority_heuristic.rs` | NEW: cadence classification unit tests |
| `tests/priority_header.rs` | NEW: header override tests |
| `tests/priority_live.rs` | NEW: end-to-end scheduler integration (two fake flows, assert the slow one wins) |
| `docs/plans/001-llm-qdisc-proxy/PRIORITY.md` | NEW: operator-facing doc explaining the heuristic and header |

Out of scope (deferred):

- Switching backpressure mode to `hybrid` (separate plan).
- `max_tokens`-based heuristic (could complement cadence later).
- Per-client allowlist of trusted header senders.
- Auto-detection of "user is typing" signals (would need harness help).

## Success criteria

- [ ] A flow that fires 3+ requests with < 2s gaps is demoted to
      `priority=10` (`background`) at the next `admit()`.
- [ ] A flow with > 30s inter-request gaps is promoted to
      `priority=100` (`interactive`).
- [ ] A cold-start flow (fewer than `min_samples` requests) keeps
      `default_priority` until the heuristic has enough data.
- [ ] A request with `X-LLM-Priority: interactive` pins the flow to
      priority 100 for all subsequent admits; the cadence heuristic is
      skipped for that flow until `X-LLM-Priority: auto` (or admin
      reset) is sent.
- [ ] `X-LLM-Priority: auto` unsets the override and the heuristic
      resumes.
- [ ] An explicitly pinned flow still gets force-admitted by the
      300s starvation mechanism if it's perpetually preempted (no
      new starvation regressions).
- [ ] `priority_policy.enabled: false` disables the heuristic; header
      overrides still apply.
- [ ] `cargo clippy --all-targets -- -D warnings`,
      `cargo build --all-targets`, `cargo test --all` pass.
- [ ] New metrics (`llm_flow_priority_class`, `llm_flow_priority_source`,
      `llm_flow_inter_request_seconds`) appear in `/metrics` output.
- [ ] Replaying the 2026-08-03 19:47 contention burst under the new
      heuristic shows interactive flows admitted within 1–2 completions
      while the batch backlog waits (manual or scripted verification).

## Task order

```
01 (config + Flow/FlowRegistry plumbing)
 → 02 (cadence module + classify logic)
 → 03 (X-LLM-Priority header resolution)
 → 04 (scheduler integration: apply priority in admit paths)
 → 05 (metrics)
 → 06 (tests: unit + integration + end-to-end)
 → 07 (docs: PRIORITY.md + WORKLOG rollup)
```

- 01 → 02 (cadence lives in FlowRegistry, needs the plumbing first)
- 04 depends on 02 and 03 (scheduler consumes both)
- 05 can run after 04 (metrics need the call sites)
- 06 depends on all prior (verifies the full chain)
- 07 is last (docs reflect final behavior, including any decisions
  surfaced during implementation)

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Heuristic thrashes a flow's priority on bursty interactive sessions (e.g. user fires 3 quick follow-ups). | `min_samples: 3` + hysteresis: promotion to interactive is sticky (don't demote back to background on a single fast burst). Add a brief "grace" before demoting a previously-interactive flow. |
| Background flows never make progress under heavy interactive load. | Existing 300s starvation timeout force-admits them. Verify with the e2e test in 06. |
| Cadence history grows unbounded for long-lived sessions. | `sample_window` caps the rolling history at 20 instants. Older entries are evicted FIFO. |
| Per-flow state across restarts. | Out of scope — heuristic re-learns within `min_samples` requests after restart. Persisting cadence state is future work. |
| Header spoofing by untrusted clients. | Pinning to `interactive` is a soft signal; the scheduler still bounds concurrency at `max_active_flows`. Operator can disable the header via `priority_policy.enabled: false` (which disables the heuristic but **not** the header; we keep the header always honored as client intent). A future plan can add per-client auth. |

## Future work (not in this plan)

- Persisting per-flow cadence state across proxy restarts (SQLite, like
  the context-compression store).
- `max_tokens` as a secondary signal (short completions = interactive,
  long generations = background).
- Per-source weighting of signals (cadence 0.7, max_tokens 0.3).
- Switching backpressure to `hybrid` for 429 responses (separate plan
  — depends on confidence in priority assignment).
- Authenticated header override (sign the `X-LLM-Priority` value).
- Adaptive thresholds (tune the gaps from observed global traffic)
  instead of static config.

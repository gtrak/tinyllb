# Plan 006 — Turn-Boundary State Machine for Interactive Detection

## Why

The median-gap heuristic shipped in Plan 004 is inert in production. A live
`/metrics` scrape on 2026-08-04 showed all 20 active flows sitting at priority
**50 (agent)** — none classified as `interactive (100)`, none as `background
(10)`. The priority machinery exists but does not discriminate.

Two root causes:

1. **Median is the wrong statistic.** A real interactive session bursts then
   waits: gaps like `[1s, 1s, 1s, 45s, 1s, 1s, 1s, 60s, 1s, 1s]`. The median
   of that sequence is **1s**, so Plan 004's classifier — median ≤ 2s →
   `background` — labels the most interactive session imaginable as the
   *lowest* priority. The median collapses gap *pattern* into one number and
   discards the very structure that signals interactivity: idle chunks
   followed by resumption.

2. **Gaps don't distinguish tool execution from human idle.** A 30s gap
   before a `role:tool` request is tool execution time (the agent is working).
   A 30s gap before a `role:user` request is a human thinking (interactive).
   Plan 004 treats them identically — any gap ≥ 30s is "interactive,"
   regardless of who initiated the next request. An agentic loop that spends
   30s running a tool call gets the same mark as a human who paused to
   think.

### The discriminator

Interactivity is **not a property of the gap distribution; it is a property
of the gap pattern**. Specifically:

- **Interactive** — a session that, at least once, goes idle (a gap ≥
  `idle_gap_threshold` at a **proper turn boundary**) and then **resumes**.
  The resumption is the proof: a human came back.
- **Agentic loop** — a session that keeps firing with no turn-boundary idles —
  continuous cadence, no human in the loop on a sub-idle timescale. These run
  past 2-3 requests (`min_samples: 3` was always too eager to lock in any
  classification).
- **One-and-done** — a single request (or short burst) that then terminates.
  We cannot observe termination positively — and it doesn't matter: while
  idle it consumes nothing, and if it ever resumes, the idle gap *is* the
  interactive signal.

The key insight: **"waiting on a tool call is not idle; it needs to be a
proper turn boundary."** The request body's `messages[].role` already tells
us who's initiating — `role: "user"` is a turn boundary; `role: "tool"` is
intra-turn. The proxy already parses the request body for flow ID resolution,
streaming detection, and max_tokens extraction. No response-side changes are
needed; the signal is available at admit time.

### Scheduler constraint

`select_best` (`src/scheduler/priority.rs:47`) compares priority with `>`only — **magnitude is ignored, only ordering matters**. DRR deficit scales
with `weight`, not priority. So a flow at priority 73 beats one at 25 exactly
the same way 100 beats 10. *Continuous priority values would have zero
scheduling effect* without changing the scheduler. The redesign therefore
uses **discrete tiers** with a confidence gradient via a state machine, not
continuous priority magnitudes.

## What

### Turn-boundary-aware state machine

A reactive per-flow state machine replaces the median-gap classifier. States
encode confidence as well as classification:

| State | Priority | Entered when |
|---|---|---|
| `Cold` | 100 (interactive) | New flow, no evidence yet (optimistic) |
| `Interactive` | 100 | ≥1 turn-boundary idle observed |
| `AgenticSuspected` | 50 (agent) | Continuous arrivals (no idle) past `agentic_suspected_threshold` |
| `AgenticConfirmed` | 10 (background) | Continuous arrivals past `agentic_confirmed_threshold` |

**Transitions (reactive, ongoing — not one-shot):**

- `Cold → Interactive`: first turn-boundary idle.
- `Cold / Interactive → AgenticSuspected`: continuous-arrival counter ≥
  `agentic_suspected_threshold` (default 5).
- `AgenticSuspected → AgenticConfirmed`: counter ≥
  `agentic_confirmed_threshold` (default 12).
- `Any → Interactive`: **immediately** on a turn-boundary idle (gap ≥
  `idle_gap_threshold` + `role: user`). An agentic loop that pauses for a
  human and resumes has proven it's interactive — this reactivity is the
  thing the median model fundamentally cannot do.
- Fast turn boundary (`role: user` but gap < `idle_gap_threshold`): resets
  the continuous-arrival counter, no state change. The user took over so the
  "continuous agentic run" is broken, but without an idle chunk there's no
  interactive promotion.

This makes "2-3 requests isn't sufficient" literally true in the model: at
2-3 continuous `role:tool` arrivals you're still `Cold` (100) or just entering
`AgenticSuspected` (50) — you are *never* confidently classified from a tiny
sample. Confidence accumulates with evidence.

**Idle chunk** = a single inter-request gap ≥ `idle_gap_threshold` (default
30s) where the **current** request's last message has `role: "user"` (or
`"system"`, or is non-JSON / non-chat — optimistic default).

**One-and-done is naturally handled:** one request at optimistic 100, then
silence (no resource use). If it ever resumes, the idle gap *is* the
interactive signal. No explicit session-end detection or stale-flow reset is
needed for V1.

### Turn-boundary detection (request side)

The proxy already parses the request body as JSON for flow ID resolution
(`proxy.rs:564`), streaming detection (`body_wants_streaming`), max_tokens
extraction (`extract_max_tokens`), and usage injection (`inject_include_usage`).
A new `is_turn_boundary_request()` function checks the last message's `role`:

| Last message `role` | `is_turn_boundary` |
|---|---|
| `"user"` | `true` (human starting a new message) |
| `"system"` | `true` (optimistic) |
| `"tool"` | `false` (agent sending a tool result — intra-turn) |
| `"assistant"` | `false` (prefill continuation — intra-turn) |
| non-JSON, no `messages`, empty array | `true` (optimistic, consistent with cold-start philosophy) |

This is a better signal than `finish_reason` because:
1. No response-side changes — nothing threads from response completion back
   to the next admit.
2. More direct: "did the *user* initiate this?" is the actual question, not
   "did the model stop?"
3. Already available at admit time — the request body is parsed in-place.

### Scheduler integration

To avoid touching ~130 test/bench call sites that invoke `admit()`, a new
`admit_with_turn_boundary()` method is added; the existing `admit()` becomes
a thin wrapper defaulting `is_turn_boundary = true` (optimistic, matching the
cold-start philosophy). Only the proxy handler calls the new method with the
actual detected value. All existing tests, benches, and scheduler internals
continue using `admit()` unchanged.

### Config

```yaml
priority_policy:
  enabled: true
  idle_gap_threshold: 30s              # gap at a turn boundary counts as "idle"
  agentic_suspected_threshold: 5      # continuous (no idle) arrivals → AgenticSuspected
  agentic_confirmed_threshold: 12      # …continuing → AgenticConfirmed
```

Removes `interactive_gap_min`, `background_gap_max`, `sample_window`,
`min_samples` (all median-model concepts). No `deny_unknown_fields` — old
config keys are silently ignored, so existing `config.yaml` files keep
working without edits.

The `Priorities` struct (`interactive: 100`, `agent: 50`, `background: 10`)
is unchanged — it maps state-machine output to the same numeric tiers the
scheduler already consumes, and the `X-LLM-Priority` header API
(`interactive`/`agent`/`background`/`auto`) is untouched.

### What this fixes

The `[1, 1, 1, 45, 1, 1, 1, 60, ...]` session — misclassified as
`background(10)` by Plan 004's median model because median = 1s — now
correctly registers two turn-boundary idles (the 45s and 60s gaps precede
`role: user` requests) and promotes to `Interactive(100)`. The continuous
`[1, 1, 1, 1, ...]` agentic loop, where every gap precedes `role: tool`,
never sees a turn-boundary idle and climbs `Cold → AgenticSuspected →
AgenticConfirmed`, ending at `background(10)` with the right *evidence*
behind it.

## Success criteria

- [ ] A flow that sends 5+ continuous `role:tool` requests with no
      turn-boundary idle is classified `AgenticSuspected` (priority 50) at
      the next `admit()`.
- [ ] Continuing to 12+ continuous `role:tool` requests promotes to
      `AgenticConfirmed` (priority 10).
- [ ] A flow with a gap ≥ 30s before a `role:user` request (a turn-boundary
      idle) is classified `Interactive` (priority 100) — regardless of
      prior state (reactive promotion).
- [ ] A gap ≥ 30s before a `role:tool` request does **not** promote — the
      gap is tool execution, not a turn boundary.
- [ ] A new flow (zero arrivals) starts at `Cold` (priority 100) on first
      admit — optimistic cold start.
- [ ] A fast turn boundary (`role:user`, gap < `idle_gap_threshold`) resets
      the continuous-arrival counter but does not promote to `Interactive`.
- [ ] `X-LLM-Priority` header pins still work and still suppress the
      heuristic for that flow.
- [ ] `priority_policy.enabled: false` disables the heuristic; header
      overrides still apply.
- [ ] Non-chat requests (`/v1/completions`, malformed body, no `messages`)
      default to `is_turn_boundary = true` (optimistic).
- [ ] `cargo clippy --all-targets -- -D warnings`,
      `cargo build --all-targets`, `cargo test --all` pass.
- [ ] `/metrics` shows varied `llm_flow_priority_class` values (100/50/10)
      under mixed traffic, not all-50 as today.
- [ ] An interactive session cycling `interactive → agentic → interactive`
      (human types, then an agentic subtask runs, then human resumes)
      correctly transitions through the state machine and returns to
      `Interactive` on the next turn-boundary idle.

## Task order

```
01 (config schema) ─────────────┐
                                ├─→ 02 (cadence rewrite) ─→ 04 (scheduler admit) ─→ 05 (tests) ─→ 07 (docs)
03 (turn-boundary detection) ───┘                                       ↗
                                                                   06 (metrics)
```

- 01 and 03 are independent — can be implemented in parallel.
- 02 depends on 01 (cadence state machine reads the new `PriorityPolicy`).
- 04 depends on 02 and 03 (scheduler threads `is_turn_boundary` from the
  proxy into the cadence registry).
- 05 depends on 04 (tests exercise the full chain).
- 06 can run after 04.
- 07 is last (docs reflect final behavior).

## Scope

| Area | Change |
|---|---|
| `src/config/mod.rs` | Replace `PriorityPolicy` fields |
| `src/config/loader.rs` | Update defaults + validation |
| `src/flow/cadence.rs` | Rewrite: state machine replaces median classifier |
| `src/gateway/proxy.rs` | Add `is_turn_boundary_request()`, call `admit_with_turn_boundary()` |
| `src/scheduler/mod.rs` | Add `admit_with_turn_boundary()`, pass `is_turn_boundary` to cadence |
| `src/metrics/mod.rs` | Add `llm_flow_cadence_state` gauge (optional) |
| `tests/priority_heuristic.rs` | Full rewrite |
| `tests/priority_live.rs` | Full rewrite |
| `tests/policy_config.rs` | Update for new config fields |
| `tests/config.rs` | Update for new config fields |
| `docs/plans/001-tinyllb/PRIORITY.md` | Rewrite |
| `docs/plans/004-interactive-priority-heuristic/PLAN.md` | Add supersession note |

### Out of scope (deferred)

- Making priority magnitude matter to the scheduler (DRR quantum ∝ priority,
  weighted random selection). The current ordinal `select_best` is
  sufficient for discrete tiers; magnitude-aware scheduling is a separate
  plan if 3 tiers prove too coarse in practice.
- Stale-flow reset (a flow idle > N minutes resets to `Cold`). The idle gap
  itself drives the right behavior; explicit reset is unnecessary for V1.
- Persisting per-flow cadence state across proxy restarts.
- `max_tokens` as a secondary signal.
- Switching backpressure mode to `hybrid`.
- Response-side `finish_reason` capture (the request-side `role` check is
  sufficient and simpler; `finish_reason` remains a fallback if the
  request-side signal proves unreliable for some frameworks).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Some frameworks send tool results as `role: "user"` instead of `role: "tool"`, causing false turn-boundary idles. | V1 trusts the `role` field per the OpenAI spec. If observed in the wild, add a content-shape heuristic (presence of `tool_call_id` → treat as tool result). Document the assumption. |
| Agentic loop that pauses > 30s between tool calls (slow tool) is correctly NOT promoted — but a human who pauses 25s (just under threshold) is also not promoted. | `idle_gap_threshold` is tunable. 30s is conservative; operators can lower it to 15s if their agentic tool chains are fast. The histogram `llm_flow_inter_request_seconds` reveals the right cutoff per deployment. |
| A burst of new flows all start at `Cold` (priority 100) and crowd out classified flows. | Bounded: each new flow does at most `agentic_suspected_threshold` (5) requests before demotion begins. The starvation timeout (300s) still protects classified flows. |
| State machine thrashes on a flow that alternates 4 tool calls + 1 user message + 4 tool calls + … (counter resets to 0 each user message, never hits threshold 5). | This is correct behavior — the user is in the loop every 5th request, so the flow *is* interactive. Priority 100 is right. |
| Removing `sample_window` / `min_samples` breaks existing `config.yaml` files that set them. | No `deny_unknown_fields` on `PriorityPolicy` — unknown keys are silently ignored by `serde`. Old configs keep working; the stale keys just have no effect. |

## Relationship to Plan 004

Plan 004 shipped the median-gap heuristic, the `X-LLM-Priority` header, the
`CadenceRegistry`, and the priority metrics. This plan **replaces the
classification logic** (median → state machine) and **replaces the config
schema** (`interactive_gap_min`/`background_gap_max`/`sample_window`/
`min_samples` → `idle_gap_threshold`/`agentic_suspected_threshold`/
`agentic_confirmed_threshold`). It **preserves** the header API, the
`Priorities` struct, the scheduler integration point, the metrics, and the
starvation guarantee from Plan 004.

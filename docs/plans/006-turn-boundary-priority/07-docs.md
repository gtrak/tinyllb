# 07 — Docs

**Parent:** `PLAN.md`  
**Depends on:** `05-tests.md`, `06-metrics.md`

## Objective

Update the operator-facing documentation (`PRIORITY.md`) to describe the new
state machine and config, and mark Plan 004 as superseded.

## Files

| File | Change |
|---|---|
| `docs/plans/001-tinyllb/PRIORITY.md` | Rewrite: state machine description, new config, turn-boundary detection, tuning guide |
| `docs/plans/004-interactive-priority-heuristic/PLAN.md` | Add supersession note at top pointing to Plan 006 |

## Steps

### 1. Rewrite `docs/plans/001-tinyllb/PRIORITY.md`

Replace the entire file. The new structure:

**Title:** Priority Heuristic — Turn-Boundary State Machine

**Overview** — The proxy classifies flows into priority tiers based on
whether the session exhibits *turn-boundary idles* (gaps at a proper turn
boundary where the human is thinking). An interactive session bursts, goes
idle at a turn boundary, and resumes. An agentic loop fires continuously
with no turn-boundary idles — its gaps are tool-execution time, not human
idle. An explicit `X-LLM-Priority` header can pin a flow to any class or
restore heuristic-driven auto-classification.

**Header: `X-LLM-Priority`** — unchanged section (the header API didn't
change). Keep the table:

| Value | Effect |
|---|---|
| `interactive` | Pin priority to 100 |
| `agent` | Pin priority to 50 |
| `background` | Pin priority to 10 |
| `auto` | Clear any prior pin; resume cadence-based classification |

**Heuristic (auto-classification)** — replace the median-gap section with:

When no header override is pinned, the heuristic classifies each flow at
admission time using a reactive state machine:

| State | Priority | Entered when |
|---|---|---|
| `Cold` | 100 | New flow, no evidence yet (optimistic) |
| `Interactive` | 100 | ≥1 turn-boundary idle observed |
| `AgenticSuspected` | 50 | Continuous arrivals (no idle) past threshold |
| `AgenticConfirmed` | 10 | Continuous arrivals past a higher threshold |

A **turn-boundary idle** is an inter-request gap ≥ `idle_gap_threshold` where
the *current* request's last message has `role: "user"` (the human is
initiating a new turn). A gap before `role: "tool"` is *tool execution
time* — the agent is working, not idle — and does not promote the flow.

**Transitions:**
- `Cold → Interactive`: first turn-boundary idle.
- `Cold / Interactive → AgenticSuspected → AgenticConfirmed`: sustained
  continuous arrivals with no idle chunk.
- `Any → Interactive`: immediately on a turn-boundary idle (reactive — an
  agentic loop that pauses for a human and resumes has proven it's
  interactive).
- Fast turn boundaries (`role: user` but gap < threshold) reset the
  continuous counter without promoting.

**Configuration:**

```yaml
priority_policy:
  enabled: true
  idle_gap_threshold: 30s
  agentic_suspected_threshold: 5
  agentic_confirmed_threshold: 12
```

| Parameter | Default | Description |
|---|---|---|
| `enabled` | `true` | Run the cadence heuristic. Header overrides still work when `false`. |
| `idle_gap_threshold` | `30s` | Gap at a turn boundary (`role: user`) that counts as "idle." |
| `agentic_suspected_threshold` | `5` | Continuous arrivals (no idle) before demoting to `AgenticSuspected` (50). |
| `agentic_confirmed_threshold` | `12` | Continuous arrivals before demoting to `AgenticConfirmed` (10). |

### Disabling the heuristic — unchanged.

**Starvation guarantee** — unchanged (300s force-admit).

**Metrics** — update the table:

| Metric | Type | Labels | Description |
|---|---|---|---|
| `llm_flow_priority_class` | Gauge | `flow_id` | Current numeric priority (100/50/10). |
| `llm_flow_cadence_state` | Gauge | `flow_id` | State machine state (0=cold, 1=interactive, 2=suspected, 3=confirmed). |
| `llm_flow_priority_source_total` | Counter | `flow_id`, `source` | How each flow's priority was set. |
| `llm_flow_inter_request_seconds` | Histogram | `flow_id` | Observed inter-request gaps per flow. |

**Tuning guide** — replace:

- **Interactive flows feel slow under batch load**: Lower
  `agentic_suspected_threshold` (e.g., 3) to demote continuous flows faster,
  or raise `idle_gap_threshold` (e.g., 15s) to catch shorter human pauses.
- **Agentic flows demote too quickly**: Raise
  `agentic_suspected_threshold` (e.g., 8) to give the heuristic more time
  to observe an idle chunk before demoting.
- **Tool execution pauses are being misclassified as idle**: Raise
  `idle_gap_threshold` above your longest tool-call duration. The
  `llm_flow_inter_request_seconds` histogram reveals the right cutoff.
- **Cold-start window**: Flows start at `Interactive (100)` optimistically.
  The window lasts `agentic_suspected_threshold` requests. Raise it to be
  more conservative; lower it to demote faster.

### 2. Mark Plan 004 as superseded

At the top of `docs/plans/004-interactive-priority-heuristic/PLAN.md`, add:

```markdown
> **Note (2026-08-04):** The median-gap classifier described in this plan
> was replaced by a turn-boundary state machine in Plan 006. The header
> API, `CadenceRegistry`, metrics, and scheduler integration from this plan
> are preserved; only the classification logic and config schema changed.
> See `docs/plans/006-turn-boundary-priority/PLAN.md`.
```

Do not edit the rest of Plan 004 — it remains as historical record.

## Verification

```bash
# No build/test verification needed — docs only.
# Proofread: ensure config example matches src/config defaults exactly.
# Ensure the header table matches src/flow/identify.rs.
# Ensure the metrics table matches src/metrics/mod.rs.
```

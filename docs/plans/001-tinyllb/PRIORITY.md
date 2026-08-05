# Priority Heuristic — Turn-Boundary State Machine

The proxy classifies flows into priority tiers based on whether the session exhibits *turn-boundary idles* (gaps at a proper turn boundary where the human is thinking). An interactive session bursts, goes idle at a turn boundary, and resumes. An agentic loop fires continuously with no turn-boundary idles — its gaps are tool-execution time, not human idle. An explicit `X-LLM-Priority` header can pin a flow to any class or restore heuristic-driven auto-classification.

## Header: `X-LLM-Priority`

| Value | Effect |
|---|---|
| `interactive` | Pin priority to 100 |
| `agent` | Pin priority to 50 |
| `background` | Pin priority to 10 |
| `auto` | Clear any prior pin; resume cadence-based classification |

The header is parsed case-insensitively. Unknown values are logged as warnings
and ignored. When present, the override **persists for the flow across all
subsequent requests** until the flow is destroyed or `auto` is sent. The
header works alongside `X-LLM-Flow-ID`; it does not alter identity resolution,
only the priority assigned to the resolved flow.

## Heuristic (auto-classification)

When no header override is pinned, the heuristic classifies each flow at
admission time using a reactive state machine:

| State | Priority | Entered when |
|---|---|---|
| `Cold` | 100 (interactive) | New flow, no evidence yet (optimistic) |
| `Interactive` | 100 | ≥1 turn-boundary idle observed |
| `AgenticSuspected` | 50 (agent) | Continuous arrivals (no idle) past `agentic_suspected_threshold` |
| `AgenticConfirmed` | 10 (background) | Continuous arrivals past `agentic_confirmed_threshold` |

A **turn-boundary idle** is an inter-request gap ≥ `idle_gap_threshold` where the *current* request's last message has `role: "user"` (the human is initiating a new turn). A gap before `role: "tool"` is *tool execution time* — the agent is working, not idle — and does not promote the flow.

**Transitions:**
- `Cold → Interactive`: first turn-boundary idle.
- `Cold / Interactive → AgenticSuspected → AgenticConfirmed`: sustained continuous arrivals with no idle chunk.
- `Any → Interactive`: immediately on a turn-boundary idle (reactive — an agentic loop that pauses for a human and resumes has proven it's interactive).
- Fast turn boundaries (`role: user` but gap < `idle_gap_threshold`) reset the continuous-arrival counter without promoting.

## Configuration

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

### Disabling the heuristic

Set `priority_policy.enabled: false` to stop cadence-based classification.
The `X-LLM-Priority` header continues to work — it is independent of the
heuristic toggle.

## Starvation guarantee

The existing `starvation_timeout` (default 300s) force-admits any queued flow
regardless of its priority class. A `background` flow will eventually be
served — it cannot be starved indefinitely by `interactive` or `agent` flows.

## Metrics

Four new Prometheus series report priority state:

| Metric | Type | Labels | Description |
|---|---|---|---|
| `llm_flow_priority_class` | Gauge | `flow_id` | Current numeric priority (100/50/10). |
| `llm_flow_cadence_state` | Gauge | `flow_id` | State machine state (0=cold, 1=interactive, 2=agentic_suspected, 3=agentic_confirmed). |
| `llm_flow_priority_source_total` | Counter | `flow_id`, `source` | How each flow's priority was set. Sources: `heuristic`, `header`, `admin`, `auto`. |
| `llm_flow_inter_request_seconds` | Histogram | `flow_id` | Observed inter-request gaps per flow. |

## Tuning guide

- **Interactive flows feel slow under batch load**: Lower `agentic_suspected_threshold` (e.g., 3) to demote continuous flows faster, or lower `idle_gap_threshold` (e.g., 15s) to catch shorter human pauses.
- **Agentic flows demote too quickly**: Raise `agentic_suspected_threshold` (e.g., 8) to give the heuristic more time to observe an idle chunk before demoting.
- **Tool execution pauses are being misclassified as idle**: This shouldn't happen — tool gaps before `role: tool` requests are never counted as idle. If it is happening, check that your client framework sends tool results with `role: "tool"` (not `role: "user"`).
- **Cold-start window**: Flows start at `Interactive (100)` optimistically. The window lasts `agentic_suspected_threshold` requests. Raise it to be more conservative; lower it to demote faster.
- **Batch flows starve too long**: Lower `starvation_timeout` (e.g., 120s). Be careful — too low defeats the priority signal and reduces the benefit of the heuristic.

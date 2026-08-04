# Priority Heuristic — Interactive-vs-Batch Scheduling

The proxy automatically classifies flows into priority tiers based on their
inter-request cadence, allowing interactive sessions to cut ahead of batch
workloads. An explicit `X-LLM-Priority` header can pin a flow to any class
or restore heuristic-driven auto-classification.

## Overview

When multiple sessions compete for GPU capacity, a round-robin scheduler
treats them equally — an interactive user waiting for a single reply queues
behind an agentic loop hammering the API. The priority heuristic solves
this by observing each flow's request cadence and adjusting its scheduling
priority in real time:

- **Slow-gapping flows** (human typing, waiting for answers) → `interactive` (100)
- **Fast-gapping flows** (agentic loops, background sync) → `background` (10)
- **Medium-gapping flows** (typical agents) → `agent` (50, default)

The `X-LLM-Priority` header lets callers override the heuristic per flow.

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
admission time based on its recent inter-request gap pattern:

1. **Cold start** (`< min_samples` arrivals): priority stays at the default
   (`agent` = 50). The heuristic has insufficient data to classify.

2. **After `min_samples` arrivals**: the median inter-request gap is computed
   over the rolling window and compared against two thresholds:

   | Median gap | Classification | Priority |
   |---|---|---|
   | `≤ background_gap_max` (2s) | `background` | 10 |
   | `≥ interactive_gap_min` (30s) | `interactive` | 100 |
   | between the two | `agent` | 50 |

3. **Hysteresis**: An `interactive` flow is only demoted when the last 3
   consecutive gaps are ALL `≤ background_gap_max`. This prevents a burst
   of quick follow-up requests from stripping interactive priority from a
   genuinely human-driven session.

## Configuration

Add this block to `~/.config/llm-qdisc/config.yaml`:

```yaml
priority_policy:
  enabled: true
  interactive_gap_min: 30s
  background_gap_max: 2s
  sample_window: 20
  min_samples: 3
```

| Parameter | Default | Description |
|---|---|---|
| `enabled` | `true` | Run the cadence heuristic. Header overrides still work when `false`. |
| `interactive_gap_min` | `30s` | Gap at or above this → `interactive` (100). |
| `background_gap_max` | `2s` | Gap at or below this → `background` (10). |
| `sample_window` | `20` | Maximum number of arrivals to retain per flow. |
| `min_samples` | `3` | Arrivals needed before classification begins. |

### Disabling the heuristic

Set `priority_policy.enabled: false` to stop cadence-based classification.
The `X-LLM-Priority` header continues to work — it is independent of the
heuristic toggle.

## Starvation guarantee

The existing `starvation_timeout` (default 300s) force-admits any queued flow
regardless of its priority class. A `background` flow will eventually be
served — it cannot be starved indefinitely by `interactive` or `agent` flows.

## Metrics

Three new Prometheus series report priority state:

| Metric | Type | Labels | Description |
|---|---|---|---|
| `llm_flow_priority_class` | Gauge | `flow_id` | Current numeric priority of each flow. |
| `llm_flow_priority_source_total` | Counter | `flow_id`, `source` | How each flow's priority was set. Sources: `heuristic`, `header`, `admin`, `auto`. |
| `llm_flow_inter_request_seconds` | Histogram | `flow_id` | Observed inter-request gaps per flow. |

## Tuning guide

- **Interactive flows feel slow under batch load**: Widen `interactive_gap_min`
  (e.g., `45s`) or tighten `background_gap_max` (e.g., `1s`) to make the
  heuristic stricter about demoting flows.

- **Batch flows starve too long**: Lower `starvation_timeout` (e.g., `120s`).
  Be careful — too low defeats the priority signal and reduces the benefit
  of the heuristic.

- **Cold-start window**: `min_samples` controls how many requests a flow
  must send before classification begins. Raise it (e.g., `5`) to make the
  heuristic more conservative; lower it (e.g., `2`) to engage classification
  faster.

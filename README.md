# tinyllb

A scheduling and robustness proxy for LLM inference.

tinyllb sits between your agents/clients and the inference backend (vLLM or
llama.cpp). It has two jobs: **fair scheduling** of concurrent flows so no
single workload monopolizes the GPU, and **robustness** — catching bad
inference behavior (degenerate completions, backend stalls, KV-cache pressure)
before it reaches the caller. It speaks the OpenAI-compatible API, so existing
clients work unchanged.

## Why

Agents and interactive users share the same GPU. Without a proxy:

- one long agent session starves interactive users;
- a model that emits a stop token mid-thinking kills the agent thread;
- a backend stall wedges every in-flight request.

tinyllb solves all three. It schedules fairly, re-sends requests that produce
degenerate completions, and detects a wedged backend before it holds all
slots.

## Architecture

```
agent / client  →  tinyllb (proxy)  →  vLLM / llama-server  →  GPU
```

The proxy intercepts OpenAI-compatible requests, classifies them into flows,
applies scheduling + robustness, and forwards to a single backend URL. It does
not care about tensor parallelism or multi-GPU details — that is the backend's
job.

## Scheduling

- **DRR fair sharing** — Deficit Round-Robin over flows with per-flow weights
  and priorities. No flow monopolizes the GPU.
- **Turn-boundary priority** — flows are automatically classified: a user who
  paused to type is *interactive* (highest priority); an agent firing rapid
  tool calls is *agentic* (lower priority). No manual configuration.
- **Starvation protection** — a queued flow that waits too long is
  force-admitted.
- **Completion bias** — while the GPU is full, new flows wait so in-flight
  work finishes first.

## Robustness

The proxy doesn't just forward bytes — it parses requests and responses, and
intercepts bad inference behavior before the caller sees it.

- **Premature-stop retry** — catches degenerate completions where the model
  emits a stop token with no content and no tool calls (e.g. Qwen emitting EOS
  mid-thinking). The proxy re-sends the request with a higher temperature,
  transparently. The agent never sees the failed attempt — the stream simply
  continues.
- **Stall watchdog** — if the backend is busy but producing no tokens for a
  configurable window, in-flight streams are aborted and new admissions are
  rejected until the stall clears. Prevents a wedged engine from holding all
  slots.
- **Transient re-forward** — pre-response backend errors (context overflow on
  a prompt that fits, backend restart) are re-sent with exponential backoff.
  The client sees a single clean response.
- **KV pressure management** — under KV-cache pressure, the proxy adapts: it
  biases admission toward flows with the largest resident KV footprint (so
  they finish and free memory), and optionally caps concurrency via a
  threshold ladder.

## Backend Support

- **vLLM** — full support. KV pressure is read from the backend's
  `/metrics` (KV cache utilization gauge). No slot concept; session pinning is
  off.
- **llama.cpp (llama-server)** — full support. KV pressure is derived from
  `/slots` (per-slot resident tokens). **Session slot pinning**: named
  sessions are deterministically pinned to a stable llama-server slot so the
  prompt KV cache is reused across turns, cutting time-to-first-token on
  follow-up turns. Auto-detected from the server's `/slots` endpoint — no
  config needed. Ephemeral requests keep auto-selection.

The proxy auto-detects the backend flavor (vLLM vs llama.cpp) from the
metrics endpoint.

## Quickstart

### 1. Run a backend

vLLM:

```bash
vllm serve meta-llama/Llama-3.2-1B-Instruct --port 8000
```

Or llama.cpp's `llama-server` with metrics and slots enabled:

```bash
llama-server --model model.gguf --metrics --slots --port 8000
```

llama.cpp notes:

- `--metrics` feeds the stall watchdog; `--slots` adds a live KV-pressure
  signal, which activates KV-aware admission, selection bias, and the
  KV-pressure concurrency cap. Without `--slots`, KV pressure stays 0 and
  those features remain inert.
- Align `scheduler.max_active_flows` with llama-server's `--parallel N` so
  the proxy admits no more concurrent flows than slots the backend can run.
- Session pinning is automatic — the slot count is read from the server's
  `/slots` endpoint, so named sessions pin to a stable slot with no config.

### 2. Start the proxy

```bash
cp config.example.yaml config.yaml          # or edit backend.url
cargo run --release
```

The proxy binds to `0.0.0.0:8080` by default and forwards to
`http://localhost:8000` (the backend).

### 3. Test

```bash
curl localhost:8080/v1/models
```

Or with `scripts/run_local.sh` for a one-shot local dev run:

```bash
chmod +x scripts/run_local.sh
./scripts/run_local.sh
```

The script starts the proxy and prints curl examples for common endpoints.

### Docker deployment (single-GPU)

```bash
docker compose up -d
curl localhost:8080/v1/models
```

This starts two services: the backend (vLLM on port 8000, GPU-reserved) and
the proxy (port 8080, forwarding to the backend). Edit `config.example.yaml`
or mount your own `config.yaml` to override settings; individual keys can also
be overridden via environment variables (see below). For multi-GPU (tensor
parallel) setups, adjust the backend service's command and GPU reservations in
`docker-compose.yaml` — the proxy needs no changes, since it forwards to the
single backend URL.

## Configuration

Copy `config.example.yaml` to `config.yaml` and edit. All values can be
overridden via environment variables (see below).

| Key | Default | Description |
| --- | --- | --- |
| `backend.url` | `http://localhost:8000` | Backend vLLM URL |
| `backend.metrics_interval` | `1s` | Interval for polling vLLM `/metrics` |
| `backend.stall_timeout` | `30s` | Inference-stall watchdog window; `0` disables |
| `backend.transient_retry.max_attempts` | `3` | Transient-error re-forward attempts; `0` disables |
| `backend.transient_retry.backoff_start` | `500ms` | First backoff delay between re-forwards |
| `backend.transient_retry.backoff_max` | `4s` | Cap on the exponential backoff delay |
| `backend.kv_unified` | `false` | llama-server runs with `-kvu` (unified KV); selects the `/slots` pressure denominator |
| `scheduler.max_active_flows` | `4` | Max concurrent flows admitted |
| `scheduler.starvation_timeout` | `300s` | Force-admit a flow after this idle time |
| `scheduler.completion_bias.enabled` | `true` | Defer new-flow admission while active flows exceed target |
| `scheduler.completion_bias.target_active_flows` | `0` | Active-flow target for completion bias (`0` = `max_active_flows`) |
| `scheduler.completion_bias.predictive_admit` | `false` | Pre-admit when an active flow has delivered ≥90% of estimated tokens |
| `scheduler.kv_bias.enabled` | `true` | KV-cache-aware selection bias among eligible waiting flows |
| `scheduler.kv_bias.bias_full_at` | `0.9` | KV fraction at which the bias fully dominates selection |
| `scheduler.kv_bias.pressure_below` | `0.5` | KV fraction below which the bias is off (pure DRR fairness) |
| `scheduler.kv_pressure.enabled` | `false` | KV-pressure-driven dynamic concurrency cap (soft: holds new admits, never preempts) |
| `scheduler.kv_pressure.thresholds` | `[]` | Threshold ladder: KV fraction >= `at` caps to `max_flows` (ascending) |
| `flows.default_weight` | `1` | Default DRR weight per flow |
| `flows.default_priority` | `50` | Default priority (higher = more urgent) |
| `flows.flow_idle_ttl` | `600s` | Evict a flow after this much idle time |
| `priorities.interactive` | `100` | Priority class for interactive sessions |
| `priorities.agent` | `50` | Priority class for agent sessions |
| `priorities.background` | `10` | Priority class for background jobs |
| `backpressure.mode` | `blocking` | `blocking`, `fail_fast`, or `hybrid` |
| `backpressure.max_queue_depth` | `100` | Max queued requests before backpressure |
| `backpressure.max_wait` | `10s` | Max time a request waits in queue |
| `backpressure.retry_after_base` | `1s` | Base `Retry-After` for backpressure rejections |
| `server.bind` | `0.0.0.0:8080` | Listen address for the proxy |
| `server.tps_window_secs` | `10` | Rolling window (seconds) for the `llm_tokens_per_second` gauge |
| `metrics.endpoint` | `/metrics` | Path serving Prometheus metrics |
| `backpressure.kv_policy.enabled` | `false` | Enable KV-cache-aware admission |
| `backpressure.kv_policy.reject_threshold` | `0.95` | Reject when KV utilization > threshold |
| `backpressure.kv_policy.delay_threshold` | `0.80` | Delay admission when KV utilization > threshold |
| `backpressure.kv_policy.bypass_interactive` | `true` | Interactive (priority-100) flows skip KV delay/reject |
| `priority_policy.enabled` | `true` | Turn-boundary priority reclassification |
| `priority_policy.idle_gap_threshold` | `30s` | Idle gap at a user turn that counts as a turn boundary |
| `priority_policy.agentic_suspected_threshold` | `5` | Continuous arrivals to suspect agentic (agent priority) |
| `priority_policy.agentic_confirmed_threshold` | `12` | Continuous arrivals to confirm agentic (background priority) |
| `retry_policy.enabled` | `false` | Enable premature-stop retry for chat completions |
| `retry_policy.max_retries` | `2` | Retry attempts after the initial (total = max_retries + 1) |
| `retry_policy.temperature_step` | `0.3` | Temperature added per retry attempt |
| `retry_policy.max_temperature` | `1.5` | Cap on bumped temperature |
| `retry_policy.default_temperature` | `0.0` | Base temperature when the request omits one |
| `request_timeout` | *(none)* | Optional per-request timeout (e.g. `300s`) |

### Environment Variables

| Variable | Description |
| --- | --- |
| `CONFIG_PATH` | Path to config file (default: `config.yaml`) |
| `PORT` | Override bind port (e.g. `PORT=9090` binds to `0.0.0.0:9090`) |
| `TINYLLB__BACKEND__URL` | Override `backend.url` |
| `TINYLLB__BACKEND__STALL_TIMEOUT` | Override stall watchdog window |
| `TINYLLB__BACKEND__KV_UNIFIED` | Override `backend.kv_unified` |
| `TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS` | Override max active flows |
| `TINYLLB__SCHEDULER__STARVATION_TIMEOUT` | Override starvation timeout |
| `TINYLLB__SCHEDULER__KV_PRESSURE__ENABLED` | Override KV pressure cap enable flag |
| `TINYLLB__FLOWS__DEFAULT_WEIGHT` | Override default flow weight |
| `TINYLLB__FLOWS__DEFAULT_PRIORITY` | Override default flow priority |
| `TINYLLB__PRIORITIES__INTERACTIVE` | Override interactive priority |
| `TINYLLB__PRIORITIES__AGENT` | Override agent priority |
| `TINYLLB__PRIORITIES__BACKGROUND` | Override background priority |
| `TINYLLB__BACKPRESSURE__MODE` | Override backpressure mode |
| `TINYLLB__BACKPRESSURE__MAX_QUEUE_DEPTH` | Override max queue depth |
| `TINYLLB__SERVER__BIND` | Override server bind address |
| `TINYLLB__BACKPRESSURE__KV_POLICY__ENABLED` | Override KV policy enable flag |
| `TINYLLB__BACKPRESSURE__KV_POLICY__REJECT_THRESHOLD` | Override KV reject threshold |
| `TINYLLB__BACKPRESSURE__KV_POLICY__DELAY_THRESHOLD` | Override KV delay threshold |
| `TINYLLB__PRIORITY_POLICY__ENABLED` | Override turn-boundary priority reclassification |
| `TINYLLB__RETRY_POLICY__ENABLED` | Override premature-stop retry enable flag |
| `TINYLLB__REQUEST_TIMEOUT` | Override request timeout |

The `TINYLLB__` prefix replaces config sections: `TINYLLB__SECTION__KEY`
maps to `section.key` in YAML.

## Metrics

`GET /metrics` serves Prometheus format.

### Scheduling

| Metric | Description |
| --- | --- |
| `llm_active_flows` | gauge, number of currently active flows |
| `llm_flow_credit` | gauge per flow, current DRR credit |
| `llm_queue_depth` | gauge, requests waiting in queue |
| `llm_queue_wait_seconds` | histogram, queue wait time |
| `llm_starvation_force_admits_total` | counter, force-admit events |

### Throughput

| Metric | Description |
| --- | --- |
| `llm_tokens_generated_total` | counter, total tokens generated |
| `llm_tokens_per_second` | gauge, approximate tokens/sec |
| `llm_request_events_total` | counter, lifecycle events (started/token/completed/cancelled) |

### Backend Health

| Metric | Description |
| --- | --- |
| `llm_backend_kv_pressure` | gauge, latest KV usage fraction (vLLM or llama.cpp /slots-derived) |
| `llm_backend_stalled` | gauge, 1 while the watchdog considers the backend deadlocked |
| `llm_kv_admission_decisions_total` | counter, KV admission decisions (accept/delay/reject) |
| `scheduler_effective_max_flows` | gauge, pressure-capped max_active_flows |
| `tinyllb_backend_stall_events_total` | counter, stalls detected |

### Robustness

| Metric | Description |
| --- | --- |
| `tinyllb_premature_stop_detected_total` | counter, premature stops detected |
| `tinyllb_premature_stop_exhausted_total` | counter, degenerate turns after retries exhausted |
| `tinyllb_backend_retries_total` | counter, transient re-forwards |
| `tinyllb_backend_retry_exhausted_total` | counter, retries exhausted |

### Priority Heuristic

| Metric | Description |
| --- | --- |
| `llm_flow_cadence_state` | gauge per flow, cadence state (0=cold, 1=interactive, 2=agentic_suspected, 3=agentic_confirmed) |
| `llm_flow_priority_class` | gauge per flow, numeric priority (100/50/10) |
| `llm_flow_inter_request_seconds` | histogram per flow, inter-request gap |

## Endpoints

| Endpoint | Method | Description |
| --- | --- | --- |
| `/healthz` | GET | Health check (returns `ok`) |
| `/metrics` | GET | Prometheus metrics |
| `/v1/models` | GET | List models (proxied to vLLM) |
| `/v1/chat/completions` | POST | Chat completions (proxied) |
| `/v1/completions` | POST | Completions (proxied) |
| `/flows` | POST | Register (or update) a flow's weight/priority |
| `/queue` | GET | Current queue state |

## Design Docs

For in-depth design docs — invariants, non-goals, rationale, and interface
contracts — see the `lat.md/` directory. Each file covers a domain:
`gateway.md` (proxy, streaming, retries), `scheduler.md` (DRR, admission),
`scheduler_policies.md` (completion bias, KV bias, pressure cap),
`backend.md` (metrics monitor, stall watchdog), `flow.md` (identity,
cadence), `admission.md` (backpressure), `metrics.md` (metric families),
`config.md` (configuration), `api.md` (admin endpoints).

## License

See LICENSE file.

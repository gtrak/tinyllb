# tinyllb

tc/qdisc for LLM inference workloads.

An OpenAI-compatible scheduling proxy that sits between your agent/client and
vLLM to enforce flow-aware scheduling (DRR), backpressure, and
KV-cache-aware admission. Designed for local-first GPU deployments.

## Architecture

```
client (agent) -> proxy (this) -> vLLM -> GPU
```

The proxy intercepts `/v1/*` requests, classifies them into flows, and applies
scheduling + backpressure before forwarding to the backend. The proxy does not
care about tensor parallelism or multi-GPU details — it forwards to a single
backend URL.

## Features

- **DRR flow scheduling** — Deficit Round-Robin over flows with per-flow
  weights, priorities, and starvation protection (force-admit after
  `scheduler.starvation_timeout`).
- **Completion bias** — while the number of active flows exceeds
  `scheduler.completion_bias.target_active_flows`, admission is deferred for
  *new* flows so in-flight work finishes first. `predictive_admit` optionally
  lets a near-done flow (≥90% of estimated tokens delivered) yield early.
- **KV selection bias** — under KV-cache pressure, the eligible waiting flow
  with the largest resident KV footprint wins the next permit, so it finishes
  and frees blocks instead of being paged in/out. Bias only: it never rejects
  or delays a request.
- **KV-cache-aware admission** (`kv_policy`) — opt-in delay/reject of
  admissions based on live vLLM KV utilization
  (`kv_policy.reject_threshold` / `kv_policy.delay_threshold`).
- **Stall watchdog** — if the backend reports busy work but token counters
  stop advancing for `backend.stall_timeout`, in-flight streams are aborted
  (retry on fresh connections) and new admissions are rejected until the stall
  clears. `0` disables the watchdog.
- **Transport retry** — a stream that ends without a terminal frame is
  treated as a transport failure: the body is aborted so the client's normal
  retry logic re-sends the request.
- **Transient backend-error re-forward** — pre-response transient errors are
  re-sent with bounded exponential backoff (`backend.transient_retry`;
  `max_attempts: 0` disables): llama.cpp intake `exceed_context_size_error`
  where the prompt fits slot capacity, and network failures from a backend
  restart. Permanent errors pass through unchanged.
- **Premature-stop retry** — degenerate `finish_reason: "stop"` chat
  responses (no content, no tool calls) are re-sent with bumped temperature;
  see [Premature-Stop Retry](#premature-stop-retry).
- **Turn-boundary priority** — a per-flow cadence state machine
  reclassifies flows: a user-turn idle gap (≥ `priority_policy.idle_gap_threshold`)
  marks the flow interactive; continuous arrivals past
  `agentic_suspected_threshold` / `agentic_confirmed_threshold` demote it to
  agent / background priority.
- **Idle-flow eviction** — flows idle longer than `flows.flow_idle_ttl` are
  reaped from the flow/cadence registries.

## Quickstart

### 1. Run vLLM locally

```bash
vllm serve meta-llama/Llama-3.2-1B-Instruct --port 8000
```

Or any OpenAI-compatible backend — e.g. llama.cpp's `llama-server` with
metrics enabled:

```bash
llama-server --model model.gguf --metrics --slots --port 8000
```

The proxy auto-detects the backend flavor (vLLM vs llama.cpp) from the
metric-name prefix on each `/metrics` scrape. llama.cpp notes:

- `--metrics` feeds the stall watchdog; `--slots` adds a live KV-pressure
  signal: the proxy polls `/slots` and derives `kv_usage` from the slots'
  resident tokens, which makes `kv_policy` (admission), `kv_bias` (selection),
  and the `kv_pressure` cap active on that backend. Without `--slots`,
  `kv_usage` stays 0.0 and those features remain inert.
- Set `backend.kv_unified: true` when llama-server runs with `-kvu` (unified
  KV pool) so the pressure denominator is the single shared pool, not the sum
  of per-slot `n_ctx`.
- `scheduler.kv_pressure` (disabled by default) maps KV pressure to a dynamic
  `max_active_flows` ceiling via a threshold ladder — a soft cap: it holds new
  admits and never preempts in-flight flows.
- Align `scheduler.max_active_flows` with llama-server's `--parallel N` so
  the proxy admits no more concurrent flows than slots the backend can run.

### 2. Start the proxy

```bash
cp config.example.yaml config.yaml          # or edit backend.url
cargo run --release
```

The proxy binds to `0.0.0.0:8080` by default and forwards to
`http://localhost:8000` (the vLLM backend).

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

## Docker Deployment (Single-GPU)

Build and deploy with docker compose:

```bash
docker compose up -d
curl localhost:8080/v1/models
```

This starts two services:
- **vllm** — vLLM OpenAI server on port 8000 (GPU-reserved).
- **proxy** — this proxy on port 8080, forwarding to the vllm service.

Edit `config.example.yaml` or mount your own `config.yaml` to override
settings. You can also override via environment variables:

```yaml
# docker-compose override example
environment:
  - TINYLLB__BACKEND__URL=http://vllm:8000
  - TINYLLB__SCHEDULER__COMPLETION_BIAS__ENABLED=false
  - TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS=8
```

### Multi-GPU Local (Tensor Parallel)

To run vLLM across multiple GPUs, change the vLLM command in
`docker-compose.yaml`:

```yaml
vllm:
  image: vllm/vllm-openai:latest
  command: --model meta-llama/Llama-3.2-1B-Instruct --tensor-parallel-size 2
  deploy:
    resources:
      reservations:
        devices:
          - driver: nvidia
            count: 2
            capabilities: [gpu]
```

The proxy does not need changes — it forwards to the single backend URL
(`http://vllm:8000`). Tensor parallelism is handled entirely by vLLM.

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
| `scheduler.max_active_flows` | `4` | Max concurrent flows admitted |
| `scheduler.starvation_timeout` | `300s` | Force-admit a flow after this idle time |
| `scheduler.completion_bias.enabled` | `true` | Defer new-flow admission while active flows exceed target |
| `scheduler.completion_bias.target_active_flows` | `0` | Active-flow target for completion bias (`0` = `max_active_flows`) |
| `scheduler.completion_bias.predictive_admit` | `false` | Pre-admit when an active flow has delivered ≥90% of estimated tokens |
| `scheduler.kv_bias.enabled` | `true` | KV-cache-aware selection bias among eligible waiting flows |
| `scheduler.kv_bias.bias_full_at` | `0.9` | KV fraction at which the bias fully dominates selection |
| `scheduler.kv_bias.pressure_below` | `0.5` | KV fraction below which the bias is off (pure DRR fairness) |
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
| `metrics.endpoint` | `/metrics` | Path serving Prometheus metrics |
| `kv_policy.enabled` | `false` | Enable KV-cache-aware admission |
| `kv_policy.reject_threshold` | `0.95` | Reject when KV utilization > threshold |
| `kv_policy.delay_threshold` | `0.80` | Delay admission when KV utilization > threshold |
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

### Endpoints

| Endpoint | Method | Description |
| --- | --- | --- |
| `/healthz` | GET | Health check (returns `ok`) |
| `/metrics` | GET | Prometheus metrics |
| `/v1/models` | GET | List models (proxied to vLLM) |
| `/v1/chat/completions` | POST | Chat completions (proxied) |
| `/v1/completions` | POST | Completions (proxied) |
| `/flows` | POST | Register (or update) a flow's weight/priority |
| `/queue` | GET | Current queue state |

## Environment Variables

| Variable | Description |
| --- | --- |
| `CONFIG_PATH` | Path to config file (default: `config.yaml`) |
| `PORT` | Override bind port (e.g. `PORT=9090` binds to `0.0.0.0:9090`) |
| `TINYLLB__BACKEND__URL` | Override `backend.url` |
| `TINYLLB__BACKEND__STALL_TIMEOUT` | Override stall watchdog window |
| `TINYLLB__SCHEDULER__MAX_ACTIVE_FLOWS` | Override max active flows |
| `TINYLLB__SCHEDULER__STARVATION_TIMEOUT` | Override starvation timeout |
| `TINYLLB__FLOWS__DEFAULT_WEIGHT` | Override default flow weight |
| `TINYLLB__FLOWS__DEFAULT_PRIORITY` | Override default flow priority |
| `TINYLLB__PRIORITIES__INTERACTIVE` | Override interactive priority |
| `TINYLLB__PRIORITIES__AGENT` | Override agent priority |
| `TINYLLB__PRIORITIES__BACKGROUND` | Override background priority |
| `TINYLLB__BACKPRESSURE__MODE` | Override backpressure mode |
| `TINYLLB__BACKPRESSURE__MAX_QUEUE_DEPTH` | Override max queue depth |
| `TINYLLB__SERVER__BIND` | Override server bind address |
| `TINYLLB__KV_POLICY__ENABLED` | Override KV policy enable flag |
| `TINYLLB__KV_POLICY__REJECT_THRESHOLD` | Override KV reject threshold |
| `TINYLLB__KV_POLICY__DELAY_THRESHOLD` | Override KV delay threshold |
| `TINYLLB__PRIORITY_POLICY__ENABLED` | Override turn-boundary priority reclassification |
| `TINYLLB__RETRY_POLICY__ENABLED` | Override premature-stop retry enable flag |
| `TINYLLB__REQUEST_TIMEOUT` | Override request timeout |

The `TINYLLB__` prefix replaces config sections: `TINYLLB__SECTION__KEY`
maps to `section.key` in YAML.

## Premature-Stop Retry

The proxy can retry `/v1/chat/completions` requests that produce degenerate
stops — responses with `finish_reason: "stop"`, empty `content`, and no
`tool_calls`. Such turns kill agentic threads because the agent sees an
empty assistant message and cannot continue.

**How it works:** On detection of a premature stop, the proxy re-sends the
exact forwarded body with `temperature` bumped by `temperature_step` per
attempt (capped at `max_temperature`), up to `max_retries` times. The retry
bypasses the scheduler (admission slot held) and skips non-chat paths.

- **Streaming:** The client sees a seamless concatenation — failed reasoning
appears as extra "thinking" but the thread survives with a single terminal
frame + `[DONE]`.
- **Non-streaming:** The client receives the good response body after retry;
fail-open forwards the last degenerate body if all retries are exhausted.
- **Environment overrides:** `TINYLLB__RETRY_POLICY__ENABLED=true`, etc.

**Disabled by default.** Enable via `retry_policy.enabled: true` in config.

**Prometheus metrics** (prefixed `tinyllb_premature_stop_`):
`detected_total`, `retries_total`, `exhausted_total`.

See `docs/plans/005-premature-stop-retry/PLAN.md` for the full design.

## Benchmarks

Measured throughput and fairness benchmarks are documented in:

- [Phase 1 Results](docs/plans/001-tinyllb/PHASE1-RESULTS.md) —
  Admission control vs direct uncontrolled path. At N=32, proxy achieves
  3.48× higher tokens/sec than direct.
- [Phase 2 Results](docs/plans/001-tinyllb/PHASE2-RESULTS.md) —
  historical Phase 2 results (fairness / no-starvation / queue-endpoint
  correctness under DRR).

## License

See LICENSE file.

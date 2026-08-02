# llm-qdisc-proxy

tc/qdisc for LLM inference workloads.

An OpenAI-compatible scheduling proxy that sits between your agent/client and
vLLM to enforce flow-aware scheduling (DRR, WFQ, FIFO), backpressure, and
KV-cache-aware admission control. Designed for local-first GPU deployments.

## Architecture

```
client (agent) -> proxy (this) -> vLLM -> GPU
```

The proxy intercepts `/v1/*` requests, classifies them into flows, and applies
scheduling + backpressure before forwarding to the backend. The proxy does not
care about tensor parallelism or multi-GPU details — it forwards to a single
backend URL.

## Quickstart

### 1. Run vLLM locally

```bash
vllm serve meta-llama/Llama-3.2-1B-Instruct --port 8000
```

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
  - LLM_QDISC__BACKEND__URL=http://vllm:8000
  - LLM_QDISC__SCHEDULER__ALGORITHM=wfq
  - LLM_QDISC__SCHEDULER__MAX_ACTIVE_FLOWS=8
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
| `scheduler.algorithm` | `drr` | Scheduling algorithm: `fifo`, `wfq`, `drr` |
| `scheduler.max_active_flows` | `4` | Max concurrent flows admitted |
| `scheduler.starvation_timeout` | `300s` | Force-admit a flow after this idle time |
| `flows.default_weight` | `1` | Default WFQ weight per flow |
| `flows.default_priority` | `50` | Default priority (higher = more urgent) |
| `priorities.interactive` | `100` | Priority class for interactive sessions |
| `priorities.agent` | `50` | Priority class for agent sessions |
| `priorities.background` | `10` | Priority class for background jobs |
| `backpressure.mode` | `blocking` | `blocking`, `fail_fast`, or `hybrid` |
| `backpressure.max_queue_depth` | `100` | Max queued requests before backpressure |
| `backpressure.max_wait` | `10s` | Max time a request waits in queue |
| `server.bind` | `0.0.0.0:8080` | Listen address for the proxy |
| `kv_policy.enabled` | `false` | Enable KV-cache-aware admission |
| `kv_policy.reject_threshold` | `0.95` | Reject when KV utilization > threshold |
| `kv_policy.delay_threshold` | `0.80` | Delay admission when KV utilization > threshold |
| `request_timeout` | *(none)* | Optional per-request timeout (e.g. `300s`) |

### Endpoints

| Endpoint | Method | Description |
| --- | --- | --- |
| `/healthz` | GET | Health check (returns `ok`) |
| `/metrics` | GET | Prometheus metrics |
| `/v1/models` | GET | List models (proxied to vLLM) |
| `/v1/chat/completions` | POST | Chat completions (proxied) |
| `/v1/completions` | POST | Completions (proxied) |
| `/flows` | GET | Active flow list |
| `/queue` | GET | Current queue state |

## Environment Variables

| Variable | Description |
| --- | --- |
| `CONFIG_PATH` | Path to config file (default: `config.yaml`) |
| `PORT` | Override bind port (e.g. `PORT=9090` binds to `0.0.0.0:9090`) |
| `LLM_QDISC__BACKEND__URL` | Override `backend.url` |
| `LLM_QDISC__SCHEDULER__ALGORITHM` | Override scheduler algorithm |
| `LLM_QDISC__SCHEDULER__MAX_ACTIVE_FLOWS` | Override max active flows |
| `LLM_QDISC__SCHEDULER__STARVATION_TIMEOUT` | Override starvation timeout |
| `LLM_QDISC__FLOWS__DEFAULT_WEIGHT` | Override default flow weight |
| `LLM_QDISC__FLOWS__DEFAULT_PRIORITY` | Override default flow priority |
| `LLM_QDISC__PRIORITIES__INTERACTIVE` | Override interactive priority |
| `LLM_QDISC__PRIORITIES__AGENT` | Override agent priority |
| `LLM_QDISC__PRIORITIES__BACKGROUND` | Override background priority |
| `LLM_QDISC__BACKPRESSURE__MODE` | Override backpressure mode |
| `LLM_QDISC__BACKPRESSURE__MAX_QUEUE_DEPTH` | Override max queue depth |
| `LLM_QDISC__SERVER__BIND` | Override server bind address |
| `LLM_QDISC__KV_POLICY__ENABLED` | Override KV policy enable flag |
| `LLM_QDISC__KV_POLICY__REJECT_THRESHOLD` | Override KV reject threshold |
| `LLM_QDISC__KV_POLICY__DELAY_THRESHOLD` | Override KV delay threshold |
| `LLM_QDISC__REQUEST_TIMEOUT` | Override request timeout |

The `LLM_QDISC__` prefix replaces config sections: `LLM_QDISC__SECTION__KEY`
maps to `section.key` in YAML.

## Benchmarks

Measured throughput and fairness benchmarks are documented in:

- [Phase 1 Results](docs/plans/001-llm-qdisc-proxy/PHASE1-RESULTS.md) —
  Admission control vs direct uncontrolled path. At N=32, proxy achieves
  3.48× higher tokens/sec than direct.
- [Phase 2 Results](docs/plans/001-llm-qdisc-proxy/PHASE2-RESULTS.md) —
  WFQ fairness, no-starvation guarantees, completion bias, and queue
  endpoint correctness.

## License

See LICENSE file.

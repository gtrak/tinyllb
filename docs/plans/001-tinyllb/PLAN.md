# Plan 001 — tinyllb

Implementation plan for the **Agent-Aware LLM Inference Scheduling Proxy**
described in `PRD.md`.  Tracks the build-out from empty repo to an MVP
scheduling proxy in front of vLLM, with phased delivery matching the PRD's
MVP scope (§12) and stretching into the V1/V2 scheduler and vLLM integration
phases.

## Why

Local LLM deployments increasingly run many concurrent agentic workloads
(coding agents, research agents, automation, chat, background jobs) against a
single constrained GPU.  Uncontrolled concurrency causes throughput
collapse, KV-cache pressure, long-tail latency, agent starvation, and partial
progress across many tasks instead of completed tasks.

The proxy applies network-scheduler abstractions (qdisc, traffic shaping, flow
scheduling) to LLM inference: each agent / conversation / workflow is a
schedulable **flow**, and the proxy performs admission control, queue
management, fairness, and completion bias so vLLM stays near optimal
utilization and valuable long-running tasks complete.  Positioning (PRD §15):
*"tc/qdisc for LLM inference workloads."*

## What

An OpenAI API-compatible reverse proxy written in **Rust** (axum + tokio +
serde + tower + prometheus), deployable as `client -> proxy -> vLLM -> GPU`.
It is **not** a replacement for vLLM's token scheduler, a model router, a
distributed cluster manager, an auth layer, or a UI dashboard (PRD §3).

### Technical approach

* **Gateway** — axum reverse proxy forwarding `/v1/chat/completions`,
  `/v1/completions`, `/v1/models` to a configured vLLM backend, preserving
  streaming and error semantics.
* **Flow identity** — derive `flow_id` from `X-LLM-Flow-ID` header, request
  `metadata.flow_id`, or an auto-generated ephemeral default.
* **Admission control** — configurable `max_active_flows` and a token-budget
  estimate (`prompt_tokens + max_output_tokens`); decision is
  `accept | delay | reject`.  KV-cache-aware admission is a Phase 3 stretch.
* **Scheduling** — phased: MVP FIFO with per-flow limits → V1 weighted fair
  queueing (token allocation ∝ weight) → V2 Deficit Round Robin (credit
  accumulation/consumption).
* **Priority + completion bias** — flow priority gates admission preference
  with starvation protection; when `active_generations > target`, new flows
  are not admitted so in-flight work completes.
* **Backpressure** — `429 Too Many Requests` with `Retry-After`, or queue
  indefinitely; modes `blocking | fail-fast | hybrid`.
* **Streaming & cancellation** — preserve SSE token streaming; release
  scheduler resources on completion, disconnect, timeout, or explicit cancel;
  restore flow credit on cancel.
* **Observability** — Prometheus metrics for queue depth, wait time, active
  flows, tokens generated, tokens/sec, flow credit, starvation seconds,
  backend active requests / errors.
* **API extensions** — optional `POST /flows` registration and `GET /queue`
  status.
* **Configuration** — YAML config file + env overrides.

### Scope

**In scope (this plan):**
* Rust workspace skeleton, CI lint/test gate, config loading.
* OpenAI-compatible gateway with streaming + error passthrough.
* Flow identification + flow registry.
* Admission controller (max-active-flows + token-budget; KV-aware deferred).
* Scheduler trio (FIFO MVP → WFQ V1 → DRR V2), priority, completion bias,
  starvation protection.
* Backpressure, streaming lifecycle, cancellation, credit restoration.
* Prometheus metrics + `/metrics` endpoint + `/queue` + `POST /flows`.
* Integration tests against a vLLM stub/mock; deployment docs / Dockerfile.

**Out of scope (deferred to future plans):**
* Model router (PRD §13) — only single backend in this plan.
* Speculative workload management (PRD §13).
* Persistent queues across proxy restart (PRD §13) — in-memory only here.
* Agent economics tracking (PRD §13).
* User authentication (PRD §3).
* UI dashboards (PRD §3).
* Distributed / multi-backend clusters (PRD §3).

## Success criteria

Taken from PRD §2 goals and §14 metrics; verified by the issue files:

* **Throughput (G1)** — aggregate tokens/sec ≥ +20% vs uncontrolled concurrent
  requests under a bursty workload benchmark; OOM/KV-cache failures near zero;
  throughput stable (low variance) under bursty load.
* **No starvation (G2)** — no flow waits longer than `starvation_timeout`
  without measurable progress; configurable fairness policies honored.
* **Agent-aware (G3)** — requests grouped into flows; scheduling happens at
  flow level with per-flow weight/priority; `GET /queue` reflects flow
  grouping.
* **API compatibility (G4)** — OpenAI clients connect unmodified to
  `/v1/chat/completions`, `/v1/completions`, `/v1/models`; responses
  byte-equivalent to vLLM for the passthrough path; streaming works.
* **Observability** — Prometheus `/metrics` exposes every metric in PRD §8;
  queue visibility is complete (`GET /queue`).
* **Operational** — proxy boots from a YAML config + env overrides; ships a
  Dockerfile and a one-command local run documented in README.

## Task order

Issue files follow the PRD's phased rollout (§12).  Numbering is grouped by
phase; within a phase, lower numbers are dependencies of higher numbers.
Cross-cutting concerns that threads through all phases (config, metrics,
deployment) get their own sequence so they can be revisited each phase rather
than front-loaded.

```
Phase 0 — Foundation
  01  Rust workspace + CI gate + lint/typecheck                (blocks all)
  02  Config schema + loader (YAML + env)                       (blocks scheduling work)

Phase 1 — Basic Queue Proxy (PRD MVP Phase 1)
  03  Reverse-proxy gateway (OpenAI routes, streaming passthrough, error semantics)
  04  Prometheus metrics + /metrics endpoint
  05  FIFO queue with max_active_flows admission control
  06  Backpressure (429 + Retry-After, blocking/fail-fast/hybrid)
  07  Phase 1 integration tests + benchmark harness

Phase 2 — Agent Scheduling (PRD V1/V2)
  08  Flow identification (header / metadata / ephemeral default) + flow registry
  09  Flow API (POST /flows) + GET /queue status
  10  Weighted Fair Queueing scheduler (V1)
  11  Deficit Round Robin scheduler (V2) with credit bookkeeping
  12  Priority system + starvation protection + completion bias
  13  Streaming lifecycle + cancellation + credit restoration
  14  Phase 2 integration tests + fairness/throughput benchmarks

Phase 3 — vLLM Integration (PRD Phase 3, stretch)
  15  KV-cache-aware admission (vLLM metrics integration)
  16  Dynamic admission + token feedback loop
  17  Phase 3 integration tests against live vLLM

Cross-cutting (revisited each phase)
  18  Logging / tracing (structured, OpenTelemetry-friendly)
  19  Dockerfile + deployment docs (local single-GPU, multi-GPU local)
```

Dependency summary:

* `01` gates everything (no code without the workspace + CI gate).
* `02` gates `05`, `08`, `10`, `11`, `12`, `15`.
* `03` gates `05` (admission wraps the proxy) and `04` (metrics instrument the proxy).
* `05` gates `06` (backpressure uses the queue) and `07` (tests need the queue).
* `08` gates `09`, `10`, `11`, `12`, `13` (all scheduling work needs flow identity).
* `10`/`11` gate `12` (priority/completion-bias sit on top of a weight-aware scheduler).
* `07`, `14`, `17` are the phase gates; a phase is "done" when its test issue closes.
* `18` and `19` are independent; land `18` early in Phase 1, `19` any time before Phase 1 ships.

Phase completion = all of that phase's issues closed AND its test/benchmark
issue (07 / 14 / 17) passing.  See per-issue Verification sections.

## Notes

* **Rationale for Rust over Python** (PRD §11): chosen for the permanent-daemon
  profile — async networking, low overhead, long-running service model — even
  at the cost of a slower MVP than a Python prototype.  The plan accepts this
  tradeoff explicitly.
* **In-memory state only** this plan; persistent queues (PRD §13) are a
  future plan.  A proxy restart drops all in-flight queues — documented as a
  known limitation.
* **vLLM stub for tests**: Phase 1/2 tests run against an in-process mock
  backend to keep CI hermetic; Phase 3 adds live-vLLM integration tests gated
  behind an env flag so CI without a GPU still passes.

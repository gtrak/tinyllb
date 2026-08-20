This directory defines the high-level concepts, business logic, and architecture of this project using markdown. It is managed by [lat.md](https://www.npmjs.com/package/lat.md) — a tool that anchors source code to these definitions. Install the `lat` command with `npm i -g lat.md` and run `lat --help`.

- [[admission]] — Scheduler admission gates: backpressure, KV-cache-aware admission, per-flow token progress
- [[api]] — Admin control-plane HTTP API: router assembly, flow registration, queue status
- [[app]] — Binary composition: crate module declarations, application startup, token rate gauge task
- [[backend]] — vLLM backend monitoring: KV-cache monitor and metrics parsing
- [[config]] — Configuration contract, loading, and validation
- [[flow]] — Flow identity, registry and state, and request flow identification
- [[gateway]] — Reverse proxy: application state, request handling, streaming passthrough, error model
- [[metrics]] — Prometheus metrics registry, export endpoint, and metric family contracts
- [[scheduler]] — Scheduler facade, DRR queueing discipline, and queue ticket
- [[scheduler_policies]] — Priority, starvation, completion bias, request lifecycle
- [[telemetry]] — Telemetry and logging initialization

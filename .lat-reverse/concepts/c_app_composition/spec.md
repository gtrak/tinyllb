# App Composition

## Purpose

App composition establishes the process lifecycle: telemetry-first initialization, configuration-driven construction of all services, wiring into shared application state, and binding to an HTTP router. It guarantees a single-process server that cannot start without valid configuration and always initializes observability before any other subsystem.

## Non-goals

This concept covers process initialization and router composition only; it deliberately excludes operational concerns handled by other layers.

- Does not provide graceful shutdown; process terminates on any fatal error
- Does not accept CLI arguments; all configuration is environment or file-based
- Does not manage multi-process or multi-node deployments
- Does not handle dynamic reconfiguration at runtime
- Does not define custom health check semantics beyond the always-success endpoint

## Interface

The composition exposes a router-construction surface, a port-resolution surface governed by environment-variable gates, and observable initialization ordering.

- Router construction accepts fully wired application state and returns a composed HTTP router ready to serve requests
- Port resolution uses a gated precedence: if `LLM_QDISC__SERVER__BIND` is set, the configured bind address is used unconditionally; otherwise `PORT` acts as an override parsed as `0.0.0.0:<port>`; absent both, the configured bind address is used
- The health endpoint responds with HTTP 200 and body `"ok"` regardless of internal application state
- Application state is shared read-only across all sub-routers except the health endpoint
- Telemetry is initialized before configuration loading; it is the first observable startup step

## Invariants

The composition maintains structural and behavioral guarantees that hold regardless of internal implementation details.

- The router always contains exactly four sub-routers: health, metrics, gateway, and admin
- The health endpoint is always reachable and always responds successfully, independent of all other application state
- All non-health sub-routers share the same application state instance
- The token-rate background task runs for the lifetime of the process; the backend monitor task runs only when KV policy is enabled
- Configuration is loaded before any service construction or network binding occurs

## Constraints

The composition operates within strict initialization and runtime boundaries derived from its single-process model.

- Single-process model: the entire server runs as one process with no fork or multiplexing
- No graceful shutdown mechanism; the server runs until a fatal error terminates the process
- All initialization failures are fatal; partial startup is not possible
- The rolling average window for rate sampling has a minimum floor of one second
- Port resolution requires valid input; malformed `PORT` values terminate the process

## Rationale

Design decisions reflect operational requirements for a deployment-dispatcher role where reliability and observability take priority over flexibility.

- Always-success health endpoint enables external health probes independent of backend state
- Fixed sub-router ordering provides predictable routing precedence for consumers
- Mandatory configuration loading prevents degraded or partial startup states
- Gated port resolution supports both container orchestration (`PORT`) and explicit binding (`LLM_QDISC__SERVER__BIND`) without ambiguity
- Telemetry-first initialization ensures all startup failures are observable

## Related

- [[?c_configuration]] — Configuration loading contract
- [[?c_http_routing]] — HTTP routing and sub-router composition
- [[?c_app_state]] — Shared application state wiring
- [[?c_background_tasks]] — Background monitoring tasks
- [[?c_health]] — Health endpoint contract
- `src/main.rs` — Entry point and composition logic

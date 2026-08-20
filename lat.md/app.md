# Library Crate Module Declarations

The crate root defines the capability boundaries of the inference gateway by declaring eight public modules, each owning a distinct domain concern.

## Purpose

Every external consumer of the crate depends on this module layout to discover and import functionality.

- The gateway organizes its capabilities into exactly eight modules, each responsible for a single domain concern.
- All public types and functions are reachable exclusively through these module declarations.
- The crate root is the sole entry point for downstream consumers; no other file exposes a public API surface.
- Each module publishes a coherent contract: its domain role is identifiable from its name alone.
- The crate root itself contributes no top-level public symbols beyond the eight module declarations.

## Non-goals

The crate root is a declaration of public surfaces, not a specification of how those surfaces collaborate at runtime.

- Internal dependencies between modules are not captured here; see individual concept specs.
- The order of module declarations carries no semantic meaning.
- Re-export granularity within each module is an implementation detail.
- Build configuration, feature flags, and dependency declarations are out of scope.
- Private helper modules and implementation-only code are not part of this contract.

## Interface

Consumers import these modules to access gateway functionality; any rewrite must preserve these eight identifiers and their domain roles.

- **`api`** — HTTP routes for flow management and queue introspection; consumers import this module to expose control-plane operations.
- **`backend`** — Snapshot-based observation of the inference engine; consumers import this module to read backend state and subscribe to state changes.
- **`config`** — Typed domain configuration and loading; consumers import this module to supply runtime parameters.
- **`flow`** — Flow classification and registry; consumers import this module to identify, create, and track request flows.
- **`gateway`** — OpenAI-compatible HTTP proxy and shared application state; consumers import this module to serve inference requests.
- **`metrics`** — Prometheus metric collectors for all gateway subsystems; consumers import this module to expose operational telemetry.
- **`scheduler`** — Flow-aware admission control and request queuing; consumers import this module to govern request prioritization.
- **`telemetry`** — Structured logging initialization; consumers import this module to configure diagnostic output.

## Invariants

The crate root enforces structural invariants about the module surface that hold regardless of implementation details.

- Exactly eight top-level public modules exist with these identifiers: `api`, `backend`, `config`, `flow`, `gateway`, `metrics`, `scheduler`, `telemetry`. Adding or removing a module changes the public API contract.
- Each module maps to a single, coherent domain concern; no module splits a domain across two names or merges two domains into one.
- All public types and functions are transitively reachable through the declared modules; no public symbol escapes the module surface.
- The configuration loading surface (`config`) is the sole mechanism for external configuration; every runtime parameter flows through the configuration module.
- Flow identification (`flow`) is the canonical flow classification mechanism; all other modules depend on flow identity established by the flow module.

## Constraints

The crate root operates within architectural limitations on how public capability boundaries are organized and extended.

- Module boundaries are stable within a release; downstream code depends on stable module paths.
- Each module owns its own public types; cross-module re-exports must be explicit in the importing module's public surface.
- The gateway must function as a standalone unit; external consumers cannot depend on internal module files directly.
- Configuration validation errors must surface at load time; invalid configurations prevent the gateway from starting.
- Public API additions require a corresponding existing module or an explicit re-export through an existing module.

## Rationale

The eight-module architecture reflects the natural decomposition of an inference proxy into independent capability domains.

- Domain-aligned modules reduce cognitive load: each module name signals its responsibility without inspection.
- Separate modules for scheduling, monitoring, and flow management prevent a single monolithic module from accumulating unrelated concerns.
- The gateway module owns the shared request lifecycle state, centralizing the coordination point that all request paths share.
- Configuration as its own module ensures a single source of truth for all runtime parameters and their validation rules.
- Metrics in a dedicated module prevents metric collectors from leaking into request-handling code.

## Related

Cross-references to related concepts and the crate root source file.

- [[api#Admin API Router Assembly]] — Admin API endpoints
- [[backend#Backend KV-Cache Monitor]] — Backend monitoring
- [[config#Configuration Loading and Validation]] — Configuration loading and types
- [[flow#Flow Registry and State]] — Flow identity and registry
- [[gateway#Reverse Proxy Request Handling]] — HTTP proxy
- [[metrics#Metrics Registry]] — Prometheus metrics
- [[scheduler#Scheduler Facade and Policy Selection]] — Request scheduling
- [[telemetry#Telemetry Initialization]] — Logging and tracing
- [[src/lib.rs]] — Crate root source

# Application Composition and Startup

App composition establishes the process lifecycle: telemetry-first initialization, configuration-driven construction of all services, wiring into shared application state, and binding to an HTTP router.

## Purpose

It guarantees a single-process server that cannot start without valid configuration and always initializes observability before any other subsystem.

## Non-goals

This concept covers process initialization and router composition only; it deliberately excludes operational concerns handled by other layers.

- Does not provide graceful shutdown; process terminates on any fatal error.
- Does not accept CLI arguments; all configuration is environment or file-based.
- Does not manage multi-process or multi-node deployments.
- Does not handle dynamic reconfiguration at runtime.
- Does not define custom health check semantics beyond the always-success endpoint.

## Interface

The composition exposes a router-construction surface, a port-resolution surface governed by environment-variable gates, and observable initialization ordering.

- Router construction accepts fully wired application state and returns a composed HTTP router ready to serve requests.
- Port resolution uses a gated precedence: if `TINYLLB__SERVER__BIND` is set, the configured bind address is used unconditionally; otherwise `PORT` acts as an override parsed as `0.0.0.0:<port>`; absent both, the configured bind address is used.
- The health endpoint responds with HTTP 200 and body `"ok"` regardless of internal application state.
- Application state is shared read-only across all sub-routers except the health endpoint.
- Telemetry is initialized before configuration loading; it is the first observable startup step.

## Invariants

The composition maintains structural and behavioral guarantees that hold regardless of internal implementation details.

- The router always contains exactly four sub-routers: health, metrics, gateway, and admin.
- The health endpoint is always reachable and always responds successfully, independent of all other application state.
- All non-health sub-routers share the same application state instance.
- The token-rate background task and the backend monitor polling task both run for the lifetime of the process; the monitor poller is spawned unconditionally so backend gauges are populated regardless of whether the KV admission policy is enabled.
- Configuration is loaded before any service construction or network binding occurs.

## Constraints

The composition operates within strict initialization and runtime boundaries derived from its single-process model.

- Single-process model: the entire server runs as one process with no fork or multiplexing.
- No graceful shutdown mechanism; the server runs until a fatal error terminates the process.
- All initialization failures are fatal; partial startup is not possible.
- The rolling average window for rate sampling has a minimum floor of one second.
- Port resolution requires valid input; malformed `PORT` values terminate the process.

## Rationale

Design decisions reflect operational requirements for a deployment-dispatcher role where reliability and observability take priority over flexibility.

- Always-success health endpoint enables external health probes independent of backend state.
- Fixed sub-router ordering provides predictable routing precedence for consumers.
- Mandatory configuration loading prevents degraded or partial startup states.
- Gated port resolution supports both container orchestration (`PORT`) and explicit binding (`TINYLLB__SERVER__BIND`) without ambiguity.
- Telemetry-first initialization ensures all startup failures are observable.

## Related

Cross-references to related concepts and the application entry point.

- [[config#Configuration Loading and Validation]] — Configuration loading contract
- [[gateway#Reverse Proxy Request Handling]] — HTTP routing and sub-router composition
- [[gateway#Gateway Application State]] — Shared application state wiring
- [[metrics#Metrics Registry]] — Background monitoring tasks
- [[api#Admin API Router Assembly]] — Health endpoint contract
- [[src/main.rs]] — Entry point and composition logic

# Token Rate Gauge Task

Background metric that derives a smoothed tokens-per-second gauge from a monotonically-increasing total token counter.

## Purpose

This concept provides an operational view of LLM throughput by converting a cumulative token counter into a rate observable.

- Derives a rolling-average tokens-per-second value from a monotonically-increasing token counter.
- Reports the rate as a public Prometheus gauge for external consumption.
- Applies configurable temporal smoothing to suppress per-request spikes.
- Operates independently of request lifecycle; requires no per-request coordination.
- Assumes the token counter never decreases under normal operation.

## Non-goals

This concept deliberately excludes several common metric capabilities to remain focused on lightweight throughput observation.

- Does not support per-backend or per-model breakdowns; reports aggregate throughput only.
- Does not expose percentiles, histograms, or distribution shape; reports a single scalar rate.
- Does not provide graceful shutdown; the task runs until process termination.
- Does not backpressure or throttle based on the measured rate; observation only.
- Does not persist state across restarts; the rolling window resets on process start.

## Interface

This concept has no callable API. Its contract is defined entirely by its metric surface and configuration inputs.

- Accepts a token counter as input; the counter must be monotonically-increasing and updated either incrementally per-token during streaming responses or at request completion for non-streaming responses.
- Accepts a configurable smoothing window specifying the averaging period, measured in seconds.
- Exposes a single Prometheus gauge (`llm_tokens_per_second`) representing the current rolling-average rate in tokens per second.
- Clamps the smoothing window to a minimum of one second; sub-second configurations are promoted to the floor.
- The gauge converges to zero as non-zero deltas age out of the window; it reports exactly zero only after the window is entirely filled with zero deltas.

## Invariants

These properties hold regardless of implementation and must survive any rewrite.

- The gauge reflects a rolling average over at most `window_secs` consecutive seconds of observation; observations older than the window are excluded from the average.
- Counter decreases produce zero delta, never negative contributions; monotonicity violations are silently absorbed.
- During warmup before the window fills, the average is computed over fewer observations than the configured window size, and the initial observation may include any pre-existing counter accumulation.
- One observation is produced each second at a fixed cadence; the cadence is independent of counter update frequency.
- The task runs indefinitely; no external termination mechanism exists.

## Constraints

These are structural limitations imposed by the current design, not fundamental properties of the concept.

- The smoothing window is unsigned; negative values are impossible at the type level; only zero requires clamping to one.
- No upper bound on window size is enforced; extremely large windows may consume excessive resources.
- The task is fire-and-forget; no handle exists to join, abort, or observe the task lifecycle externally.
- The gauge reflects a point-in-time snapshot; concurrent reads return the last-written average.
- The token counter must provide consistent read access; inconsistent reads during concurrent increments could produce incorrect deltas.

## Rationale

These design choices reflect the operational needs of LLM monitoring, where smooth throughput trends matter more than precise per-request measurement.

- A rolling average over a cumulative counter avoids per-request instrumentation, keeping the measurement path decoupled from the request path.
- Treating counter decreases as zero-delta events prevents negative throughput values that would poison the average.
- Dividing by actual observation count during warmup preserves mathematical correctness rather than padding with zeros.
- A fixed one-second observation cadence keeps the gauge update rate predictable and independent of counter update frequency.
- Fire-and-forget execution matches the monitoring use case: the task observes but does not participate in request handling.

## Related

Cross-references to related concepts and source locations for token rate observation.

- [[telemetry#Telemetry Initialization]] — Metrics registry and gauge lifecycle
- [[metrics#Metric Family Contracts]] — Counter and gauge type system
- [[config#Configuration Loading and Validation]] — Configuration injection for smoothing window
- [[gateway#Reverse Proxy Request Handling]] — Token counter increment site (batch update at request completion)
- [[gateway#Streaming Passthrough and Token Accounting]] — Token counter increment site (per-token streaming updates)
- [[src/metrics/mod.rs#Metrics]] — Counter and gauge declarations
- [[src/config/mod.rs#Server]] — Window configuration field
- [[src/gateway/proxy.rs#proxy_handler]] — Counter increment site (non-streaming)
- [[src/gateway/stream.rs#MetricStream]] — Counter increment site (streaming)

# Idle-Flow Reaper

A background reaper periodically evicts idle flows and cadence entries to bound registry growth as session identifiers accumulate.

## Purpose

The reaper caps unbounded growth of the flow and cadence registries without an explicit unregistration API.

- `src/main.rs:135-149` spawns a 60-second interval task (skip-missed-tick behavior) that calls `Scheduler::reap_idle(ttl)` on each tick.
- `Scheduler::reap_idle` calls `FlowRegistry::reap_idle(ttl)`, which removes every flow whose `depth` is 0, `active` is 0, and `last_seen` is older than `now − ttl`; it returns the number of flows removed.
- It also calls `CadenceRegistry::reap_idle(ttl)` to evict cadence entries idle for the same duration, and logs `flows_removed` and `cadence_removed` at `tracing::debug` when either is non-zero.
- The `flows.flow_idle_ttl` configuration value (default 600 seconds) sets the idle cutoff.
- Evicted flows are not a special state: a later request re-creates the flow with defaults on lookup.

## Related

Cross-references to related concepts and source locations for the idle-flow reaper.

- [[src/main.rs]] — Reaper task spawn site
- [[src/scheduler/mod.rs#Scheduler#reap_idle]] — Scheduler-level eviction entry point
- [[src/flow/mod.rs#FlowRegistry]] — Flow registry eviction
- [[config#Configuration Contract]] — `flows.flow_idle_ttl` configuration
- [[flow#Flow Registry and State]] — Registry state that the reaper bounds

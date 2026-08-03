# c_crate_root — Crate Root & Module Structure

## Purpose

The crate root defines the capability boundaries of the inference gateway by declaring eight public modules, each owning a distinct domain concern. Every external consumer of the crate depends on this module layout to discover and import functionality.

- The gateway organizes its capabilities into exactly eight modules, each responsible for a single domain concern.
- All public types and functions are reachable exclusively through these module declarations.
- The crate root is the sole entry point for downstream consumers; no other file exposes a public API surface.
- Each module publishes a coherent contract: its domain role is identifiable from its name alone.
- The crate root itself contributes no top-level public symbols beyond the eight module declarations.

## Non-goals

The crate root does not describe or guarantee internal wiring between modules. It is a declaration of public surfaces, not a specification of how those surfaces collaborate at runtime.

- Internal dependencies between modules are not captured here; see individual concept specs.
- The order of module declarations carries no semantic meaning.
- Re-export granularity within each module is an implementation detail.
- Build configuration, feature flags, and dependency declarations are out of scope.
- Private helper modules and implementation-only code are not part of this contract.

## Interface

The public surface consists of eight module declarations, each exposing a distinct capability domain. Consumers import these modules to access gateway functionality; any rewrite must preserve these eight identifiers and their domain roles.

- **`api`** — HTTP routes for flow management and queue introspection; consumers import this module to expose control-plane operations.
- **`backend`** — Snapshot-based observation of the inference engine; consumers import this module to read backend state and subscribe to state changes.
- **`config`** — Typed domain configuration and loading; consumers import this module to supply runtime parameters.
- **`flow`** — Flow classification and registry; consumers import this module to identify, create, and track request flows.
- **`gateway`** — OpenAI-compatible HTTP proxy and shared application state; consumers import this module to serve inference requests.
- **`metrics`** — Prometheus metric collectors for all gateway subsystems; consumers import this module to expose operational telemetry.
- **`scheduler`** — Flow-aware admission control and request queuing; consumers import this module to govern request prioritization.
- **`telemetry`** — Structured logging initialization; consumers import this module to configure diagnostic output.

## Invariants

The crate root enforces structural invariants about the module surface that hold regardless of implementation details. These invariants define the shape that any rewrite must preserve.

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

The eight-module architecture reflects the natural decomposition of an inference proxy into independent capability domains. Each module owns a single concern, enabling isolated evolution and clear dependency tracking.

- Domain-aligned modules reduce cognitive load: each module name signals its responsibility without inspection.
- Separate modules for scheduling, monitoring, and flow management prevent a single monolithic module from accumulating unrelated concerns.
- The gateway module owns the shared request lifecycle state, centralizing the coordination point that all request paths share.
- Configuration as its own module ensures a single source of truth for all runtime parameters and their validation rules.
- Metrics in a dedicated module prevents metric collectors from leaking into request-handling code.

## Related

- [[?c_api]] — Admin API endpoints
- [[?c_backend]] — Backend monitoring
- [[?c_config]] — Configuration loading and types
- [[?c_flow]] — Flow identity and registry
- [[?c_gateway]] — HTTP proxy
- [[?c_metrics]] — Prometheus metrics
- [[?c_scheduler]] — Request scheduling
- [[?c_telemetry]] — Logging and tracing
- [[src/lib.rs]] — Crate root source

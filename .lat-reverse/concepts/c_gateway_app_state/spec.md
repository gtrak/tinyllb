# c_gateway_app_state

## Purpose

The gateway application state aggregates all runtime dependencies required by the proxy layer to forward OpenAI-compatible API requests to a vLLM backend. Every request handler reads from the same state instance, so the state must be shareable across concurrent handler invocations and provide stable access to the HTTP client, backend target, observability, scheduling, and flow control subsystems.

## Non-goals

The application state is not responsible for request processing logic, backend discovery, or dynamic reconfiguration.

- Does not validate the backend target; validation is deferred to the caller or downstream layers.
- Does not define proxy semantics; that belongs to [[?proxy_handler]].
- Does not own lifecycle management; the state is assembled externally and injected into the router.
- Does not govern backpressure policy; it only carries backpressure configuration by value.
- Does not negotiate TLS; the default client uses the platform TLS store without customization.

## Interface

The gateway module exposes the application state struct, a router factory, a client factory, and three public sub-modules defining error types, proxy logic, and streaming support.

### State object

- Provides read access to the HTTP client, backend URL, metrics, scheduler, flow registry, backpressure configuration, and an optional per-request timeout.
- Is publicly cloneable, so that each clone provides equivalent read access to the same logical resources.
- Has no public constructor; callers construct it incrementally, which permits partial initialization before injection.

### Router factory

- Takes no arguments and produces a router exposing three OpenAI-compatible endpoints: `POST /v1/chat/completions`, `POST /v1/completions`, and `GET /v1/models`.
- Binds all three endpoints to a single proxy delegation point.
- Returns a router typed for `AppState` shared state; the caller must supply the state after construction before serving.

### Client factory

- Produces an HTTP client with a fixed global timeout and platform-default TLS configuration.
- Accepts no parameters; all configuration is baked into the factory.
- Panics if the client builder fails for any reason; this is an unrecoverable startup error.

### Sub-modules

- The `error` sub-module defines gateway-specific error types used by proxy handlers.
- The `proxy` sub-module contains the unified request proxying logic shared across all routes.
- The `stream` sub-module defines streaming support for SSE-based response forwarding.

## Invariants

The following properties hold for any conformant implementation.

### Shareability

- The state object is always cloneable so that concurrent handler invocations each carry their own instance.
- Cloning the state is shallow — references to heavyweight resources are not duplicated.

### Timeout semantics

- The per-request timeout is optional; when absent, no timeout is enforced at the proxy layer beyond whatever bound the HTTP client itself enforces.
- The per-request timeout, when present, applies uniformly to both streaming and non-streaming responses.

### Endpoint delegation

- All mounted OpenAI-compatible routes delegate to a single handler; divergent routing implies a contract violation.

## Constraints

The design has several hard boundaries that limit flexibility.

- State assembly is caller-controlled; no constructor enforces completeness, allowing inconsistent or partially initialized states to propagate into handlers.
- The HTTP client timeout is fixed at 300 seconds; callers cannot adjust it through the factory.
- The backend URL carries no built-in reachability check; unreachable backends surface as downstream proxy errors rather than startup failures.
- Backpressure configuration is cloned by value with every state clone; it is not an indirection point for live policy updates.
- System TLS is the only TLS configuration; callers cannot supply custom certificates or disable verification through the factory.

## Rationale

The shared-state model centralizes cross-cutting concerns behind a single object so that handlers do not need per-request dependency lookups.

- A mix of shared references and direct values keeps cloning lightweight: heavyweight resources are shared, and lightweight configuration is copied directly.
- No enforced construction order avoids forcing a single initialization site, enabling tests and integration points to construct only the subset of state they exercise.
- Optional per-request timeout permits callers to distinguish between bounded workloads and long-running inference tasks without altering the client-level safety net.
- A single delegation target for all routes simplifies proxy logic and reduces the surface area for handler divergence.

## Related

- [[?proxy_handler]] — the unified handler that all routes delegate to
- [[?scheduler]] — request scheduling decisions referenced by state
- [[?metrics]] — observability counters accessed through shared ownership
- [[?flow_registry]] — flow control registry carried in state
- [[?backpressure]] — backpressure configuration held by value in state
- [[src/gateway/mod.rs#AppState]] — exported state struct definition
- [[src/gateway/mod.rs#create_router]] — router factory function
- [[src/gateway/mod.rs#build_client]] — HTTP client factory function
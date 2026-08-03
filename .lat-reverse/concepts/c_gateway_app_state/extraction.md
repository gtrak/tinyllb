# c_gateway_app_state — Extraction

## Responsibilities

- Shared application state object carrying HTTP client, backend target, metrics, scheduler, flow registry, backpressure config, and optional request timeout; cloned and shared across all request handlers.
- Factory for an OpenAI-compatible router mounting chat completions (POST), completions (POST), and models (GET), all delegating to a single proxy handler.
- Factory for a default HTTP client with a 300-second timeout and system TLS.

## Interface Surfaces

### `AppState` (exported struct, `src/gateway/mod.rs:17-28`)

| Aspect | Detail |
|---|---|
| **Purpose** | Domain object holding all runtime dependencies required by proxy handlers. |
| **Inputs** | None — struct fields are populated by external callers; no public constructor. |
| **Outputs** | Provides read-access to: HTTP client, backend URL, metrics, scheduler, flow registry, backpressure settings, and optional request-level timeout. |
| **Errors** | None on construction (no fallible constructor exposed). |
| **Evidence** | Lines 17-28: `#[derive(Clone)] pub struct AppState { ... }` with seven public fields. |

### `create_router` (exported function, `src/gateway/mod.rs:30-43`)

| Aspect | Detail |
|---|---|
| **Purpose** | Produces an HTTP router binding OpenAI-compatible endpoints to a unified proxy handler. |
| **Inputs** | None (zero-argument). |
| **Outputs** | `Router<AppState>` — router requiring `AppState` as shared state. |
| **Errors** | None (infallible return). |
| **Evidence** | Lines 30-43: `pub fn create_router() -> Router<AppState>` mounting three routes via `Router::new().route(...)`. |

### `build_client` (exported function, `src/gateway/mod.rs:46-51`)

| Aspect | Detail |
|---|---|
| **Purpose** | Produces an HTTP client with default timeout and TLS configuration. |
| **Inputs** | None (zero-argument). |
| **Outputs** | `reqwest::Client` with 300-second global timeout. |
| **Errors** | Panics if the underlying client builder cannot construct a client with default TLS. |
| **Evidence** | Lines 46-51: `pub fn build_client() -> reqwest::Client` with `.timeout(Duration::from_secs(300))` and `.expect("reqwest client should build with default TLS")`. |

## Invariants

- `AppState` is `Clone` (line 17), enabling it to be shared across multiple axum handler contexts.
- `backend_url`, `metrics`, `scheduler`, and `flow_registry` are held behind `Arc` (lines 20-23), guaranteeing shared ownership without copying internal payloads.
- `backpressure` is held by value (line 24), not behind `Arc` — it is clone-copied when `AppState` is cloned.
- `request_timeout` is `Option<Duration>` (line 27); when `None`, no per-request timeout is enforced by `AppState`.
- All three mounted routes (`/v1/chat/completions`, `/v1/completions`, `/v1/models`) delegate to the identical handler symbol `proxy_handler` (lines 40-42).
- The HTTP client produced by `build_client` has a fixed 300-second timeout (line 48); it is not configurable via parameters.

## Failure Modes

- `build_client` panics at startup if the HTTP client builder fails (line 50 — `expect` call with unwrap semantics).
- `AppState` carries no public constructor; callers assemble it field-by-field, allowing a partially initialized or internally inconsistent state to exist.
- `request_timeout` absence (`None`) means long-running backend requests are unbounded at the proxy layer, only limited by the 300-second client timeout.
- `backend_url` is `Arc<Url>` with no validation logic visible in this module; an unreachable or malformed backend URL produces downstream proxy errors rather than startup failures.

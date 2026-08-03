# c_crate_root — Crate Root & Module Structure

## Responsibilities

- Declares the public module surface of the crate: `api`, `backend`, `config`, `flow`, `gateway`, `metrics`, `scheduler`, `telemetry`.
- Serves as the entry point for `extern crate` consumers; all public types and functions are reachable through these eight modules.

## Interface Surfaces

### Module layout (`src/lib.rs` lines 1–8)

| Module | Public role | Key exported types / functions |
|---|---|---|
| `api` | Admin HTTP router | `pub fn create_router() -> Router<AppState>` (line 14, `src/api/mod.rs`) |
| `backend` | Backend monitoring & KV-cache parsing | `pub struct BackendSnapshot`, `pub struct BackendMonitor`, `pub fn parse_snapshot(body: &str) -> ParseSnapshotResult`, `pub const METRIC_KV_USAGE`, `METRIC_KV_USAGE_V1`, `METRIC_KV_FREE`, `METRIC_NUM_PREEMPTION` |
| `config` | Configuration types & loader | `pub struct Config`, `pub struct Backend`, `pub struct Scheduler`, `pub struct Flows`, `pub struct Priorities`, `pub struct Backpressure`, `pub struct Metrics`, `pub struct Server`, `pub struct KvPolicyConfig`, `pub enum Algorithm`, `pub enum BackpressureMode`, `pub struct CompletionBias`, `pub use loader::load` |
| `flow` | Flow identity, registry, queue snapshots | `pub struct FlowId`, `pub struct Flow`, `pub struct FlowRegistry`, `pub struct FlowRegistration`, `pub struct QueueSnapshot`, `pub struct QueueFlowEntry`, `pub mod identify` |
| `gateway` | HTTP proxy & shared app state | `pub struct AppState`, `pub fn create_router() -> Router<AppState>`, `pub fn build_client() -> reqwest::Client`, `pub mod error`, `pub mod proxy`, `pub mod stream` |
| `metrics` | Prometheus collector registry | `pub struct Metrics`, `pub fn create_metrics() -> Arc<Metrics>`, `pub mod backend`, `pub mod endpoint`, `pub mod queue`, `pub mod throughput` |
| `scheduler` | Flow-aware request scheduling | `pub struct Scheduler`, `pub enum BackpressureRejected`, `pub struct DrrScheduler`, `pub struct WfqScheduler`, `pub struct FifoScheduler`, `pub struct QueueTicket`, `pub struct KvPolicy`, `pub struct FlowProgressTracker`, `pub struct AccountingReport`, `pub fn fail_fast_retry_after`, `pub fn mode_label`, `pub mod lifecycle` |
| `telemetry` | Structured logging initialization | `pub fn init()` |

### `api` module — Admin endpoints (`src/api/mod.rs`)

- `POST /flows` — Register or update a flow's weight and priority. Returns `201 Created` for new flows, `200 OK` for updates, `400 Bad Request` for invalid input.
- `GET /queue` — Returns current queue state: active count, waiting count, per-flow positions.

### `backend` module — Backend monitoring (`src/backend/mod.rs`)

- `BackendMonitor::new(config, metrics, client)` — Creates a monitor handle and optional background polling task. Returns `(monitor, Option<JoinHandle<()>>)`.
- `BackendMonitor::snapshot(&self)` — Returns the latest `BackendSnapshot` or `None` if channel closed.
- `BackendMonitor::wait_for(&self, predicate)` — Async predicate-based wait on snapshot state.
- `BackendMonitor::empty()` — Returns a disabled monitor with default snapshot (kv_usage=0.0, kv_free=1.0).
- `parse_snapshot(body: &str) -> ParseSnapshotResult` — Parses Prometheus text-format metrics body; returns snapshot + found flags.

### `config` module — Configuration (`src/config/mod.rs`, `src/config/loader.rs`)

- `load()` — Loads configuration from YAML file (`$CONFIG_PATH`, defaults to `config.yaml`) with `LLM_QDISC__*` env overrides. Returns `anyhow::Result<Config>`.
- `Config` — Top-level configuration containing `backend`, `scheduler`, `flows`, `priorities`, `backpressure`, `metrics`, `server`, `request_timeout`, `kv_policy`.
- Validation is enforced on load: `max_active_flows > 0`, `starvation_timeout > 0`, `default_weight > 0`, `backend.url` must be absolute, `metrics_interval > 0`, KV thresholds in valid ranges with `delay < reject`.

### `flow` module — Flow identity & registry (`src/flow/mod.rs`, `src/flow/identify.rs`)

- `FlowId::new(id)` — Creates a typed flow identifier.
- `FlowId::is_ephemeral(&self)` — Returns `true` if ID starts with `"ephemeral-"`.
- `FlowId::metric_label(&self)` — Returns metric-safe label (`"ephemeral"` for ephemeral IDs, exact ID otherwise).
- `FlowRegistry::get_or_create(id)` — Atomically returns or creates a flow with default weight/priority.
- `FlowRegistry::register(reg)` — Upserts a flow with explicit weight/priority. Returns `true` if newly created.
- `FlowRegistry::queue_snapshot(active, waiting, wait_order)` — Builds a `QueueSnapshot` from queue state.
- `identify::resolve(headers, body)` — Resolves flow ID with precedence: `X-LLM-Flow-ID` header > `metadata.flow_id` in JSON body > auto-generated ephemeral ID.

### `gateway` module — Proxy & app state (`src/gateway/mod.rs`, `src/gateway/proxy.rs`, `src/gateway/error.rs`, `src/gateway/stream.rs`)

- `AppState` — Shared request state: `client`, `backend_url`, `metrics`, `scheduler`, `flow_registry`, `backpressure`, `request_timeout`.
- `create_router()` — Creates OpenAI-compatible router: `POST /v1/chat/completions`, `POST /v1/completions`, `GET /v1/models`.
- `build_client()` — Builds a reqwest client with 300s default timeout.
- `ProxyError` — Error enum with HTTP response mapping: `BackendError` (forwarded), `Network` (502), `Internal` (500), `TooLarge` (413), `Rejected` (429 with Retry-After), `Timeout` (408).
- `proxy_handler` — Single handler for all gateway routes; proxies to backend with flow identification, admission control, token accounting, and streaming passthrough.

### `metrics` module — Prometheus collectors (`src/metrics/mod.rs`)

- `Metrics::new()` — Creates a fully registered `Metrics` instance with all collectors.
- `create_metrics()` — Returns `Arc<Metrics>`.
- Collector families: queue (`queue_depth`, `queue_wait_seconds`, `active_flows`), throughput (`tokens_generated_total`, `tokens_per_second`), backend (`requests_active`, `errors_total`), backpressure (`backpressure_rejections_total`), scheduling (`flow_credit`), starvation (`flow_starvation_seconds`, `starvation_force_admits_total`), lifecycle (`request_events_total`), KV cache (`vllm_kv_cache_usage`, `vllm_kv_cache_free`, `kv_admission_decisions_total`).

### `scheduler` module — Request scheduling (`src/scheduler/mod.rs`)

- `Scheduler::new(algorithm, ...)` — Full constructor accepting all policy parameters.
- `Scheduler::new_with_defaults(algorithm, ...)` — Backward-compatible constructor with default policies.
- `Scheduler::admit(flow_id, work_unit)` — Admits a request; returns `Result<QueueTicket, BackpressureRejected>`. KV policy gate runs before flow scheduling.
- `Scheduler::queue_depth(&self)` — Returns total waiting count (flow queue + KV-delayed).
- `Scheduler::queue_snapshot(&self)` — Returns `QueueSnapshot` with active, waiting (including KV-delayed), and per-flow positions.
- `Scheduler::service_done(flow_id)` — Returns total service_done for WFQ; 0.0 for others.
- `Scheduler::credit(flow_id)` — Returns DRR credit; 0 for non-DRR algorithms.
- `Scheduler::report_accounting(flow_id, report)` — Reports accounting for completed/cancelled requests.

### `telemetry` module — Logging (`src/telemetry/mod.rs`)

- `init()` — Initializes `tracing_subscriber` with `RUST_LOG` filter (default `info,llm_qdisc_proxy=debug`). JSON output when `LLM_QDISC_LOG_JSON=1`.
- OTLP export is scaffolded as dead code (`init_otlp()` stub).

## Invariants

- **Flow ID uniqueness**: Each `FlowId` instance is a distinct string wrapped in a newtype; ephemeral IDs are always prefixed `ephemeral-` (`src/flow/mod.rs` lines 14–38, `src/flow/identify.rs` lines 16–70).
- **Flow ID resolution precedence**: Header `X-LLM-Flow-ID` always overrides body metadata, which overrides ephemeral generation (`src/flow/identify.rs` lines 16–31).
- **Ephemeral metric label aggregation**: Ephemeral flow IDs always map to the metric label `"ephemeral"` to prevent cardinality explosion (`src/flow/mod.rs` lines 33–38, `src/flow/identify.rs` line 148).
- **KV cache snapshot monotonicity**: `parse_snapshot` derives `kv_free = 1.0 - kv_usage` when `kv_free` metric is absent but `kv_usage` is present (`src/backend/mod.rs` lines 148–150).
- **Metrics interval validity**: A zero `metrics_interval` disables the backend monitor poll loop and no background task is spawned (`src/backend/mod.rs` lines 202–214).
- **Config validation on load**: `load()` rejects configurations where `max_active_flows == 0`, `starvation_timeout` is zero, `default_weight <= 0`, `backend.url` is not absolute, `metrics_interval` is zero, or KV thresholds violate ordering (`src/config/loader.rs` lines 99–164).
- **Request body size limit**: Requests exceeding 32 MiB are rejected with `ProxyError::TooLarge` (413) (`src/gateway/proxy.rs` lines 21, 221–250).
- **Backpressure rejection response**: Rejected requests return 429 with integer `Retry-After` header (ceil to seconds per RFC 7231) (`src/gateway/error.rs` lines 67–83).
- **Queue depth includes KV delays**: `Scheduler::queue_depth()` sums both the underlying scheduler queue and KV-policy delayed count (`src/scheduler/mod.rs` lines 275–282).
- **Scheduler algorithm dispatch**: `admit()` delegates to exactly one of FIFO/WFQ/DRR based on config; other methods return algorithm-specific or no-op results (`src/scheduler/mod.rs` lines 232–333).
- **Lifecycle RAII semantics**: `LifecycleGuard` emits `request_started` on construction and `request_completed` or `request_cancelled` on drop; credit restoration reflects `estimated - delivered` (`src/scheduler/lifecycle.rs` lines 77–204).
- **Streaming passthrough preserves byte order**: `MetricStream` feeds chunks through `TokenAccumulator` without buffering or reordering (`src/gateway/stream.rs` lines 169–201).

## Failure Modes

- **Backend unreachable**: `BackendMonitor` poll loop logs warning and retains last snapshot; does not reject admission decisions (`src/backend/mod.rs` lines 256–260).
- **Metrics parse failure**: Unrecognized or malformed metric lines are silently skipped; defaults apply (`src/backend/mod.rs` lines 123–157).
- **KV cache data unavailable**: When `kv_usage` metric is absent, defaults to 0.0 (accept-all); `found_usage` flag in `ParseSnapshotResult` distinguishes absence from zero (`src/backend/mod.rs` lines 60–83).
- **Config file missing**: `load()` uses all defaults if `config.yaml` (or `$CONFIG_PATH`) does not exist (`src/config/loader.rs` line 88 — `.required(false)`).
- **Config validation failure**: `load()` returns `anyhow::Error` for invalid configurations; the proxy fails to start (`src/config/loader.rs` lines 99–164).
- **Body too large**: Request body exceeding 32 MiB triggers immediate 413 rejection (`src/gateway/proxy.rs` lines 221–250).
- **Backpressure rejection**: When scheduler admits fail, caller receives `BackpressureRejected` with `retry_after` duration, mapped to 429 response (`src/gateway/proxy.rs` lines 286–298, `src/gateway/error.rs` lines 67–83).
- **Request timeout**: Configured `request_timeout` covers connect + response phase; timeout returns 408 and drops `LifecycleGuard` as cancelled (`src/gateway/proxy.rs` lines 331–365).
- **Network error**: Backend connection failures return 502 Bad Gateway and increment `errors_total` (`src/gateway/proxy.rs` lines 335–349, `src/gateway/error.rs` lines 56–59).
- **Backend error forwarding**: 4xx/5xx responses from backend are returned verbatim with filtered headers (`src/gateway/proxy.rs` lines 383–401).
- **Streaming disconnect**: When client disconnects mid-stream, `MetricStream` drops, `LifecycleGuard` emits `request_cancelled`, credit is restored (`src/gateway/stream.rs` lines 169–201, `src/scheduler/lifecycle.rs` lines 130–203).
- **Token parsing failure**: Missing or malformed `completion_tokens` in response body results in charging full estimated cost (`src/scheduler/lifecycle.rs` lines 169–184).
- **Token overrun debit**: When backend delivers more tokens than `max_tokens` estimate, additional debit is applied (`src/scheduler/lifecycle.rs` lines 159–168).

## Related

- `[[?c_api]]` — Admin API endpoints
- `[[?c_backend]]` — Backend monitoring
- `[[?c_config]]` — Configuration loading and types
- `[[?c_flow]]` — Flow identity and registry
- `[[?c_gateway]]` — HTTP proxy
- `[[?c_metrics]]` — Prometheus metrics
- `[[?c_scheduler]]` — Request scheduling
- `[[?c_telemetry]]` — Logging and tracing

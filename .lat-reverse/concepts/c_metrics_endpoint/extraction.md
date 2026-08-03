# Extraction: Metrics Endpoint (`c_metrics_endpoint`)

## Responsibilities

- Serves Prometheus text-format metrics over HTTP via a single GET endpoint.
- Encodes all registered metric families from a shared `prometheus::Registry` into a single response body.
- Returns `200 OK` with `text/plain; version=0.0.4` on success.
- Returns `500 Internal Server Error` when metric encoding fails.
- Logs encoding errors at `ERROR` level before returning `500`.

## Interface Surfaces

### HTTP Endpoint: `GET /metrics`

- **Path**: `/metrics`
- **Method**: `GET`
- **Authentication**: None required
- **Inputs**: None accepted (no query parameters, no request body, no headers consumed)
- **Success output**: `200 OK` with `Content-Type: text/plain; version=0.0.4` header; body contains Prometheus text-format 0.0.4 encoding of all registered metric families
- **Error output**: `500 Internal Server Error` with no body; occurs only when the Prometheus text encoder fails to serialize metric families
- **Evidence**: `[[src/metrics/endpoint.rs#12]]` (handler signature), `[[src/main.rs#16]]` (route registration), `[[src/metrics/endpoint.rs#19]]` (500 path), `[[src/metrics/endpoint.rs#25-27]]` (Content-Type header), `[[src/metrics/endpoint.rs#18]]` (error log)

## Invariants

- The response body is always valid Prometheus text-format 0.0.4 or the response is `500`. There is no partial or malformed output. (`[[src/metrics/endpoint.rs#15-29]]`)
- The `Content-Type` header is always set to exactly `text/plain; version=0.0.4` on success; it is not conditional on request headers or other inputs. (`[[src/metrics/endpoint.rs#24-27]]`)
- All metric families are sourced from a single shared `Registry`; every call to `gather()` returns the current snapshot of all registered collectors. (`[[src/metrics/endpoint.rs#14]]`)
- No request data (headers, body, query string) influences the response content; the output is a function of the current registry state alone. (`[[src/metrics/endpoint.rs#12]]`)
- Encoding failures produce an `ERROR`-level log entry before the `500` response is returned. (`[[src/metrics/endpoint.rs#18]]`)

## Failure Modes

- **Encoder failure**: The Prometheus `TextEncoder` may fail to serialize metric families (e.g., malformed collector state). When this occurs, the endpoint returns `500 Internal Server Error` with an empty body. Evidence: `[[src/metrics/endpoint.rs#17-20]]`.
- **Registry inconsistency**: If collectors in the registry are in an invalid state (e.g., unregistered collectors, mismatched label cardinality), `gather()` may produce data that the encoder rejects. This manifests as `500`.
- **No other defined failure paths**: `Registry::gather()` returns `Vec<MetricFamily>` directly (not a `Result`), so it cannot propagate errors. The only fallible operation in the handler is `encode_to_string`.

## Related

- `[[src/metrics/mod.rs#16]]` — `Metrics` struct holding the shared registry and all collectors
- `[[src/gateway/mod.rs#18]]` — `AppState` carrying `Arc<Metrics>` into every handler
- `[[src/main.rs#16]]` — Route registration at `/metrics`

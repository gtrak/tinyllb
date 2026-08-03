# Metrics Endpoint (`c_metrics_endpoint`)

## Purpose

The metrics endpoint provides a read-only observation surface for all registered Prometheus metrics. It exposes the complete state of the metric registry in a standardized exposition format, enabling external systems to scrape operational data without affecting system behavior.

- Serves all registered metric families through a single HTTP endpoint
- Produces output conforming to the Prometheus text exposition format version 0.0.4
- Requires no authentication or authorization to access
- Accepts no client-supplied request data; response content is determined solely by current metric registry state

## Non-goals

The metrics endpoint does not provide mutation, filtering, or streaming capabilities.

- Does not accept, modify, or delete metrics
- Does not filter, aggregate, or transform metric data on request
- Does not support push-based metric ingestion
- Does not expose internal system state beyond registered metric collectors

## Interface

The metrics endpoint exposes a single HTTP surface with strict input/output contracts.

### HTTP GET `/metrics`

- Accepts no client-supplied request parameters, headers, or body; all request data is ignored
- Returns `200 OK` with `Content-Type: text/plain; version=0.0.4` containing all metric families when encoding succeeds
- Returns `500 Internal Server Error` with empty body only when metric serialization fails

## Invariants

The endpoint guarantees all-or-nothing consistency between successful responses and the metric registry state.

- A successful response always contains valid Prometheus text exposition format; partial or malformed output never returns with `200`
- The response body reflects the complete snapshot of all currently registered metric families at the time of the request
- The `Content-Type` header is set to `text/plain; version=0.0.4` only on successful responses; error responses do not include a `Content-Type` header
- Serialization failures produce an `ERROR`-level log entry before the `500` response is returned; this log is an observable artifact of the failure path
- Metric serialization failures produce `500 Internal Server Error`; there is no degraded fallback mode

## Constraints

The endpoint is limited by the capabilities of the underlying metric registry and serialization layer.

- Only metrics registered in the shared `[[?metric-registry]]` are visible; unregistered collectors do not appear
- Serialization failures are terminal for the response; there is no fallback format or degraded output mode
- The endpoint operates asynchronously; clients receive either the complete metric set or an error response
- No rate limiting or caching is applied at the endpoint layer

## Rationale

The design prioritizes simplicity and observability compliance over extensibility.

- A single endpoint with no parameters eliminates ambiguity about what clients expect
- Strict all-or-nothing output prevents partial metric data from misleading scrapers
- The Prometheus exposition format is the standard interface for metric scraping; deviation would break ecosystem tooling
- Unauthenticated access reflects the read-only, non-sensitive nature of aggregate metrics
- Logging encoding failures aids operational diagnosis without masking errors from scrapers

## Related

- `[[?metric-registry]]` — Shared registry supplying metric families to the endpoint
- `[[?app-state]]` — Application state object carrying the metric registry into request handlers
- `[[src/metrics/mod.rs]]` — Metric registry and collector definitions
- `[[src/metrics/endpoint.rs]]` — Endpoint handler implementation
- `[[src/gateway/mod.rs]]` — Request state carrying metrics into handlers
- `[[src/main.rs]]` — Route registration

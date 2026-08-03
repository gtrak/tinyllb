# c_backend_monitor

## Purpose

The backend monitor observes resource utilization of a vLLM inference backend and exposes a current-state view for downstream admission decisions.

- Parses raw Prometheus metrics text into typed resource observations with per-metric presence information.
- Periodically polls a backend endpoint and publishes updated observations via a watch channel.
- Exposes the latest observation to multiple concurrent readers; always returns the latest observation, including after channel closure.
- Blocks callers until an observation satisfies an arbitrary condition, terminating without distinguishing predicate-satisfied from channel-closed.
- Publishes metric-name constants so callers can validate their own deployments.

## Non-goals

The monitor is a passive observer; it does not influence scheduling or resource allocation.

- Does not make admission or scheduling decisions.
- Does not aggregate metrics across multiple backends.
- Does not validate metric accuracy beyond syntactic parsing.
- Does not expose historical metric series or trends.
- Does not handle metric schema evolution beyond the known name variants.

## Interface

The monitor exposes four contractual surfaces: parsing with presence tracking, observation publication, conditional blocking, and construction.

- **Parsing**: Accepts raw Prometheus text bodies and returns a typed observation containing KV cache utilization, KV cache availability, cumulative preemption count, and boolean flags indicating whether utilization and availability were present in the source. Lines not matching known metric names are silently ignored.
- **Observation publication**: Publishes the latest parsed observation on a watch channel. The snapshot accessor always returns the latest observation — including after channel closure, where the last published value is retained. Multiple readers may observe concurrently without coordination.
- **Conditional blocking**: Callers may suspend until an observation satisfies a caller-supplied predicate. Terminates when the predicate is satisfied or the channel is closed; the caller cannot distinguish which condition caused termination.
- **Construction**: Three constructors are available. The empty constructor yields a monitor with a static default observation and no background task. The receiver constructor wraps an existing watch receiver. The standard constructor returns the monitor alongside an optional background task handle; a missing handle indicates monitoring is disabled.
- **Metrics reporting**: Each successful poll writes utilization and availability into external Prometheus gauges for downstream consumption.

## Invariants

Published observations maintain mathematical consistency under defined conditions and degrade gracefully when data is missing.

- When utilization data is present but availability data is absent and utilization is less than 1.0, availability is derived as `1.0 − utilization`.
- When utilization equals 1.0 and availability data is absent, the default availability value is retained; the observation may represent utilization = 1.0 and availability = 1.0 simultaneously.
- The default observation always represents zero resource pressure: utilization = 0, availability = 1.0, preemptions = 0.
- Monitoring errors never produce new observations; the last known observation is preserved.
- Both utilization name variants (v0 and v1) map to the same semantic field; if both appear, the last-parsed value prevails.

## Constraints

The monitor operates within the limits of the Prometheus scraping model and HTTP availability.

- Observes a single backend instance; no multi-backend aggregation.
- Relies on Prometheus text exposition format; other formats are not supported.
- Polling cadence is fixed per configuration; adaptive intervals are not supported.
- A zero polling interval disables monitoring entirely, leaving the monitor in static-default mode.
- Metric values are not range-checked; values outside `[0..1]` are accepted without validation.

## Rationale

Every design decision trades observability completeness against admission latency.

- Single-writer / multi-reader semantics allow many admission callers to read concurrently without synchronization overhead.
- Best-effort preemption tracking prevents metric-absence from blocking admission.
- Predicate-based blocking lets callers express arbitrary conditions without polling loops.
- Deriving availability from utilization ensures compatibility with backends that omit the free gauge, but the derivation guard at the utilization = 1.0 boundary prevents negative results.
- Per-metric presence flags distinguish "metric absent" from "metric present with zero value," enabling correct derivation logic.
- Preserving the last observation on error prevents transient outages from resetting the admission view.
- Indistinguishable termination for conditional blocking avoids exposing channel lifecycle to callers that only care about predicate satisfaction.

## Related

- Source: `[[src/backend/mod.rs]]`
- Concept: `[[?vllm-metrics-endpoint]]`
- Concept: `[[?admission-controller]]`

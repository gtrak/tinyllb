# Backend KV-Cache Monitor

The backend monitor observes resource utilization of a vLLM inference backend and exposes current-state observations for downstream admission decisions.

## Purpose

The monitor parses raw Prometheus metrics into typed resource observations and publishes them via a watch channel for concurrent consumers.

- Parses raw Prometheus metrics text into typed resource observations with per-metric presence information.
- Periodically polls a backend endpoint and publishes updated observations via a watch channel.
- Exposes the latest observation to multiple concurrent readers, always returning the most recent value including after channel closure.
- Blocks callers until an observation satisfies an arbitrary condition, without distinguishing predicate-satisfied from channel-closed.
- Publishes metric-name constants so callers can validate their own deployments.

## Non-goals

The monitor itself makes no admission or scheduling decisions; its only influence on scheduling is the stall signal, which downstream layers consume to reject new admissions and abort in-flight streams (see [[backend#Inference Stall Watchdog]]).

- Does not make admission or scheduling decisions directly; it only publishes observations and a stall signal.
- Does not aggregate metrics across multiple backends.
- Does not validate metric accuracy beyond syntactic parsing.
- Does not expose historical metric series or trends.
- Does not handle metric schema evolution beyond the known name variants.

## Interface

The monitor exposes five contractual surfaces: parsing with presence tracking, observation publication, conditional blocking, stall-signal publication, and construction.

- **Parsing**: Accepts raw Prometheus text bodies and returns a typed observation containing KV cache utilization, KV cache availability, cumulative preemption count, cumulative prompt-token and generation-token counters, engine running-request and waiting-request counts, and boolean flags indicating whether utilization and availability were present. Unknown lines are silently ignored.
- **Observation publication**: Publishes the latest parsed observation on a watch channel. The snapshot accessor always returns the latest value, including after channel closure where the last published value is retained. Multiple readers observe concurrently without coordination.
- **Conditional blocking**: Callers suspend until an observation satisfies a caller-supplied predicate. Terminates when the predicate is satisfied or the channel is closed; the caller cannot distinguish which caused termination.
- **Stall signal**: `stall_receiver()` returns a `tokio::sync::watch::Receiver<bool>` that reads `true` while the inference watchdog considers the engine deadlocked; see [[backend#Inference Stall Watchdog]].
- **Construction**: Three constructors exist. The empty constructor yields a static default observation with no background task. The receiver constructor wraps an existing watch receiver. The standard constructor returns the monitor alongside an optional background task handle; a missing handle indicates monitoring is disabled.
- **Metrics reporting**: Each poll that finds the KV-usage metric in the scrape writes utilization and availability into external Prometheus gauges and runs the stall watchdog. A successful scrape that lacks the usage metric publishes nothing and writes no gauges.

## Invariants

Published observations maintain mathematical consistency under defined conditions and degrade gracefully when data is missing.

- When utilization is present but availability is absent and utilization is less than 1.0, availability is derived as `1.0 − utilization`.
- When utilization equals 1.0 and availability is absent, the default availability value is retained; the observation may simultaneously show utilization = 1.0 and availability = 1.0.
- The default observation always represents zero resource pressure: utilization = 0, availability = 1.0, preemptions = 0, and all token counters and request counts = 0.
- Monitoring errors never produce new observations; the last known observation is preserved.
- Both utilization name variants (v0 and v1) map to the same semantic field; the last-parsed value prevails.

## Constraints

The monitor operates within the limits of the Prometheus scraping model and HTTP availability.

- Observes a single backend instance; no multi-backend aggregation.
- Relies on Prometheus text exposition format; other formats are unsupported.
- Polling cadence is fixed per configuration; adaptive intervals are not supported.
- A zero polling interval disables monitoring entirely, leaving the monitor in static-default mode.
- Metric values are not range-checked; values outside `[0..1]` are accepted without validation.

## Rationale

Every design decision trades observability completeness against admission latency.

- Single-writer / multi-reader semantics allow many admission callers to read concurrently without synchronization overhead.
- Best-effort preemption tracking prevents metric-absence from blocking admission.
- Predicate-based blocking lets callers express arbitrary conditions without polling loops.
- Deriving availability from utilization ensures compatibility with backends that omit the free gauge; the derivation guard at utilization = 1.0 prevents negative results.
- Per-metric presence flags distinguish "metric absent" from "metric present with zero value," enabling correct derivation logic.
- Preserving the last observation on error prevents transient outages from resetting the admission view.
- Indistinguishable termination for conditional blocking avoids exposing channel lifecycle to callers that only care about predicate satisfaction.

## Related

Concepts and source files related to the backend KV-cache monitor.

- Source: `[[src/backend/mod.rs]]`
- Concept: The vLLM backend metrics endpoint providing Prometheus-format data scraped by this monitor
- Concept: `[[admission#KV-Cache-Aware Admission Gate]]`

# vLLM Metrics Parsing

The parser converts Prometheus text-format metrics from the vLLM backend into typed, concurrent snapshots of KV-cache state with last-value semantics.

## Purpose

The parser maps known vLLM metric names to structured fields, derives missing values where safe, and exposes the result to concurrent consumers.

- Parses Prometheus text-format metrics from the vLLM backend into a typed snapshot of KV-cache state.
- Maps known vLLM metric names to structured fields with per-metric presence tracking.
- Derives the free-fraction value from usage when the free gauge is absent and usage is less than one.
- Exposes parsed snapshots to concurrent consumers with last-value semantics.

## Non-goals

The parser focuses on typed observation, not lifecycle management or schema evolution.

- Version negotiation or schema discovery against the backend; metric names are fixed constants.
- Error reporting for unparseable input; malformed lines and unknown metrics are silently ignored.
- Preemption presence tracking in parse results; only usage and free carry boolean flags.
- Poller lifecycle management; spawn and terminate semantics belong to the monitor constructor contract, not the parsing contract.

## Interface

The concept exposes four contract surfaces: metric-name constants, typed snapshots, parse results, and a concurrent monitor handle.

- **Metric name constants**: Eight string constants declare the metric identifiers that the parser recognizes. Callers may depend on these for instrumentation or configuration alignment.

- **Usage gauge (v0)** — the v0 engine KV cache usage fraction.
- **Usage gauge (v1)** — the v1 engine KV cache usage fraction.
- **Free gauge** — the primary free-fraction gauge.
- **Preemption counter** — the cumulative preemption counter.
- **Generation tokens counter** — cumulative decode tokens; a frozen value while requests are running is the inference-deadlock signal.
- **Prompt tokens counter** — cumulative prefill tokens; tracked so all-prefill workloads are not misclassified as stalled.
- **Requests running gauge** — requests currently scheduled on the engine.
- **Requests waiting gauge** — requests queued for the engine.

- **Typed snapshot**: The snapshot is a value type representing backend state at a single point in time. It is cloneable for independent concurrent access.

- Carries seven quantities: KV usage fraction, KV free fraction, cumulative preemptions, cumulative prompt tokens, cumulative generation tokens, running request count, and waiting request count.
- The default snapshot represents a zero-load baseline: usage is zero, free is one, and all other quantities are zero.
- `is_busy()` reports whether the engine has queued or running work: true when either the running or waiting request count is positive.

- **Parse result contract**: Every parse operation yields a snapshot with boolean flags for the usage and free gauges. The result is cloneable and defaults to the zero-load baseline.

- Accepts any string and returns a fully populated result with a snapshot and presence flags.
- Unknown or unrecognized metric names are silently ignored; no error is produced.
- Malformed lines (missing name boundary, non-numeric value) are silently skipped.
- A flag value of `true` means the corresponding metric was present in the input; `false` means it was absent.
- No flag exists for the preemption gauge; callers infer preemption presence from the snapshot value and default semantics.

- **Monitor constructor contract**: The monitor provides concurrent read access to the latest snapshot. The handle is cloneable for independent reads.

- **Disabled constructor**: Produces a monitor with no background polling. Reads always return the default snapshot.
- **Receiver-backed constructor**: Wraps an existing watch receiver, enabling arbitrary snapshot injection for testing.
- **Polling constructor**: Returns a monitor handle and an optional task handle. Zero-interval produces no task; a present task signifies a live background poller.

- **Monitor access contract**: The monitor provides two access primitives for reading and waiting on snapshots.

- **Latest-value read**: Always succeeds and returns the last written value, including after the sender has been dropped. No sentinel distinguishes active from closed state.
- **Predicate-gated block**: Blocks until a caller-supplied predicate is satisfied or the channel closes. Both outcomes produce the same unit return; callers cannot distinguish satisfaction from closure.

## Invariants

The parser guarantees deterministic derivation and precedence rules across all parse operations.

- **kv_free derivation**: When the usage gauge is present, the free gauge is absent, and usage is strictly less than one, the free fraction is derived as `1.0 - usage`. If usage is one or greater, derivation is skipped and free remains at its previous value.
- **Dual usage names unify**: Both v0 and v1 usage metric names write to the same usage quantity in the snapshot. No distinction is preserved about which engine variant supplied the value.
- **Last occurrence wins**: When multiple lines match the same metric constant, the last parsed value overwrites the field. Earlier occurrences are discarded without indication.
- **Preemption truncation**: Preemption values are truncated toward zero. Fractional preemption values from the backend produce integer counts in the snapshot.
- **Disabled monitor baseline**: A disabled monitor — constructed via the disabled constructor or a zero-interval polling constructor — always returns the default snapshot: zero usage, one free, and zero token counters and request counts.
- **Default preemptions is zero**: When the preemption metric is absent from the input, the preemptions field remains at zero. This is indistinguishable from a backend reporting zero preemptions.

## Constraints

The parser and monitor impose boundaries on acceptable input and runtime behavior.

- Only Prometheus text exposition format is recognized (metric-name and value pairs per line).
- Malformed lines are silently skipped without error indication.
- Unknown metric names are ignored without indication.
- Polling uses skip-on-miss behavior for interval scheduling; missed ticks are dropped.
- Channel send failures during polling are silently absorbed; no error propagates to consumers.
- HTTP failures (unreachable backend, body read errors) retain the last successful snapshot without resetting to defaults.

## Rationale

Design choices prioritize correct derivation and concurrent access over exhaustive tracking.

Boolean flags exist only for usage and free because these gauges interact via derivation logic: callers need to distinguish "usage present, free derived" from "both present independently." Preemptions has no derivation counterpart and defaults to zero, so a presence flag adds no decision value. The latest-value read never returns a sentinel because the watch channel always yields the last written value. The predicate-gated block returns unit because the caller's predicate is the sole truth source. Clone on the snapshot and handle types is necessary for last-value broadcast semantics: each concurrent consumer must own an independent copy.

## Related

Concepts and source files related to parsing and monitoring of vLLM backend metrics.

- Source: `[[src/backend/mod.rs#BackendSnapshot]]`
- Source: `[[src/backend/mod.rs#parse_snapshot]]`
- Source: `[[src/backend/mod.rs#BackendMonitor]]`
- Concept: `[[admission#KV-Cache-Aware Admission Gate]]` — KV-cache policy consuming parsed observations
- Concept: `[[backend#Backend KV-Cache Monitor]]` — monitor construction and lifecycle contracts

# Inference Stall Watchdog

The watchdog inside the monitor polling loop detects backend inference stalls and signals the scheduler and streaming layer to reject new work and abort in-flight streams.

## Purpose

The watchdog turns "engine busy but making no token progress" into an actionable signal for the admission and streaming layers.

- `stall_receiver()` (src/backend/mod.rs:259-261) returns a `tokio::sync::watch::Receiver<bool>` that reads `true` while the watchdog considers the engine deadlocked; the scheduler and streaming tasks select on this signal.
- The watchdog runs inside the polling loop (src/backend/mod.rs:326-389) and is evaluated only on polls whose scrape contains the KV-usage metric.
- A stall is declared when the engine reports running or waiting requests (`is_busy()`, src/backend/mod.rs:98-100) and neither the prompt-token nor generation-token counters advance for `backend.stall_timeout` (default 30 seconds; a zero timeout disables the watchdog).
- On a newly detected stall the watchdog sets the stall signal to `true`, logs a warning, and increments `tinyllb_backend_stall_events_total`; on recovery it sets the signal back to `false` and logs the clearance. `llm_backend_stalled` is set to 1 while stalled and 0 otherwise (see [[metrics#Metric Family Contracts]]).
- While the engine is idle (no running or waiting requests), the watchdog resets its progress timer so an idle gap is never inherited as a stall by later work.
- While the stall signal is `true`, the scheduler rejects new admissions with 429 and a 5-second `Retry-After` (src/scheduler/mod.rs:242-247).
- Streaming tasks observe the signal and abort the in-flight backend stream when it trips, so the client retries on fresh connections (src/gateway/stream.rs).

## Related

Cross-references to related concepts and source locations for the inference stall watchdog.

- [[src/backend/mod.rs]] — Watchdog implementation inside the monitor polling loop
- [[src/backend/mod.rs#BackendMonitor#stall_receiver]] — Stall signal accessor
- [[config#Configuration Contract]] — `backend.stall_timeout` configuration
- [[metrics#Metric Family Contracts]] — Stall gauge and event counter
- [[scheduler#Scheduler Facade and Policy Selection]] — Admission gate consuming the stall signal
- [[gateway#Premature-Stop Retry]] — Retry behavior for aborted streams

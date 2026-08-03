# c_backend_metrics_parser

## Purpose

This concept defines how Prometheus text-format metrics from the vLLM backend are parsed into a typed, concurrent snapshot of KV-cache state. The parser maps known vLLM metric names to structured fields, derives missing values where safe, and exposes the result to concurrent consumers with last-value semantics.

## Non-goals

- Version negotiation or schema discovery against the backend (metric names are fixed constants).
- Error reporting for unparseable input (malformed lines and unknown metrics are silently ignored).
- Preemption presence tracking in parse results (only usage and free carry boolean flags).
- Poller lifecycle management (spawn and terminate semantics belong to the monitor constructor contract, not the parsing contract).

## Interface

The concept exposes four contract surfaces: metric-name constants, typed snapshots, parse results, and a concurrent monitor handle.

### Metric name constants

Four string constants declare the metric identifiers that the parser recognizes. Callers may depend on these for instrumentation or configuration alignment.

- **Usage gauge (v0)** identifies the v0 engine KV cache usage fraction.
- **Usage gauge (v1)** identifies the v1 engine KV cache usage fraction.
- **Free gauge** identifies the primary free-fraction gauge.
- **Preemption counter** identifies the cumulative preemption counter.

### Typed snapshot

The snapshot is a value type representing backend state at a single point in time. It is cloneable — every consumer receives its own independent copy for concurrent access without coordination.

- The snapshot carries three quantities: KV usage fraction, KV free fraction, and cumulative preemptions.
- The default snapshot represents a zero-load baseline: usage is zero, free is one, preemptions is zero.

### Parse result contract

Every parse operation yields a snapshot together with boolean flags for the usage and free gauges. The parse result is cloneable and defaults to the zero-load baseline, enabling test harnesses to construct results without invoking the parser. A flag value of `true` means the corresponding metric name was present in the input body; `false` means it was absent. No flag exists for the preemption gauge — callers must infer preemption presence from the snapshot value and default semantics.

- The parse operation accepts any string and returns a fully populated result with a snapshot and presence flags.
- Unknown or unrecognized metric names are silently ignored; no error is produced.
- Malformed lines (missing name boundary, non-numeric value) are silently skipped.

### Monitor constructor contract

The monitor provides concurrent read access to the latest snapshot. The handle is cloneable — distributing copies to multiple consumers enables independent reads without coordination. Three construction modes exist.

- **Disabled constructor** produces a monitor with no background polling. The latest-value read always returns the default snapshot.
- **Receiver-backed constructor** wraps an existing watch receiver, enabling arbitrary snapshot injection for testing.
- **Polling constructor** returns a monitor handle and an optional task handle. When the configured poll interval is zero, the task handle is absent, indicating disabled monitoring. A present task handle signifies a live background poller that publishes updates periodically.

### Monitor access contract

- **Latest-value read** returns the most recently published snapshot. The read always succeeds and returns the last written value, including after the sender has been dropped. No sentinel distinguishes active from closed state.
- **Predicate-gated block** accepts a predicate over snapshots and blocks until the predicate is satisfied or the channel closes. Both outcomes produce the same unit return; callers cannot distinguish satisfaction from closure via the return value.

## Invariants

The parser guarantees deterministic derivation and precedence rules across all parse operations.

### kv_free derivation

When the usage gauge is present, the free gauge is absent, and usage is strictly less than one, the free fraction is derived as `1.0 - usage`. If usage is one or greater, derivation is skipped and free remains at its previous value.

### Dual usage names unify

Both v0 and v1 usage metric names write to the same usage quantity in the snapshot. The parser preserves no distinction about which engine variant supplied the value.

### Last occurrence wins

When multiple lines match the same metric constant, the last parsed value overwrites the field. Earlier occurrences are discarded without indication.

### Preemption truncation

Preemption values are truncated toward zero. Fractional preemption values from the backend produce integer counts in the snapshot.

### Disabled monitor baseline

A disabled monitor — constructed via the disabled constructor or a zero-interval polling constructor — always returns the default snapshot: zero usage, one free, zero preemptions.

### Default preemptions is zero

When the preemption metric is absent from the input, the preemptions field remains at zero. This is indistinguishable from a backend reporting zero preemptions.

## Constraints

The parser and monitor impose boundaries on acceptable input and runtime behavior.

- Only Prometheus text exposition format is recognized (metric-name and value pairs per line).
- Malformed lines are silently skipped without error indication.
- Unknown metric names are ignored without indication.
- Polling uses skip-on-miss behavior for interval scheduling; missed ticks are dropped.
- Channel send failures during polling are silently absorbed; no error propagates to consumers.
- HTTP failures (unreachable backend, body read errors) retain the last successful snapshot without resetting to defaults.

## Rationale

Boolean flags are provided only for usage and free because these gauges interact via derivation logic: callers need to distinguish "usage present, free derived" from "both present independently." Preemptions has no derivation counterpart and defaults to zero, so a presence flag adds no decision value. The latest-value read never returns a sentinel because the watch channel's borrow primitive always yields the last written value — callers that need lifecycle detection must use other mechanisms. The predicate-gated block returns unit because the caller's predicate is the only truth source — if the predicate is true on wake, the condition holds regardless of whether it held before channel closure. Clone on the snapshot and handle types is necessary for last-value broadcast semantics: each concurrent consumer must own an independent copy without coordination.

## Related

- [[src/backend/mod.rs#BackendSnapshot]]
- [[src/backend/mod.rs#parse_snapshot]]
- [[src/backend/mod.rs#BackendMonitor]]
- [[?c_kv_policy]]
- [[?c_backend_monitor_lifecycle]]

# 08 — Flow Identification + Flow Registry

**Phase:** 2 (Agent Scheduling)
**Depends on:** `02`, `05`.
**Blocks:** `09`, `10`, `11`, `12`, `13`.

## Objective

Implement the core **flow** abstraction (PRD §5, §6.2).  Every request now
gets a `flow_id` resolved from, in order:
1. `X-LLM-Flow-ID` request header,
2. `metadata.flow_id` in the JSON body (best-effort parse; skipped for
   non-JSON bodies — e.g. `/v1/models`),
3. an auto-generated ephemeral `flow_id` so unrecognized clients still map to
   a unique flow.

Flows become the unit of scheduling from this issue onward.  A flow registry
stores per-flow weight, priority, and runtime credit; `08` seeds it with
defaults from config, and `09` lets clients register flows explicitly.

## Files

| File | Change |
| --- | --- |
| `src/flow/mod.rs` | New: `FlowId`, `Flow`, `FlowRegistry` (DashMap + RwLock). |
| `src/flow/identify.rs` | New: header -> metadata -> ephemeral resolution. |
| `src/gateway/proxy.rs` | Edit: resolve `flow_id` per request; attach to `Ticket`. |
| `src/scheduler/fifo.rs` | Edit: queue grouped per `flow_id` (still FIFO within a flow). |
| `tests/flow_identify.rs` | New: header wins, then metadata, then ephemeral. |

## Steps

1. `FlowId: String` newtype with sane Debug/Eq. `Flow { id, weight, priority,
   credit: AtomicI64, enqueued_at: Option<Instant> }`.
2. `FlowRegistry` keyed by `FlowId`, backed by `DashMap`.  `get_or_create`
   returns the existing flow or inserts one with `default_weight` /
   `default_priority` from config (`02`).  `credit` zero-initialized (real
   bookkeeping starts in `11`).
3. Resolution order in `identify::resolve(req)`:
   * header `X-LLM-Flow-ID` (if non-empty),
   * body `metadata.flow_id` (if body is JSON and field present),
   * `ephemeral-{UUIDv4}`.
4. `acceptance` per PRD §6.2: header takes precedence even if metadata also
   present.
5. Scheduler edit: `admit(req)` now resolves `flow_id` first; the `QueueTicket`
   carries the `FlowId`.  FIFO is still global (this issue doesn't change
   scheduling order), but `llm_queue_depth` becomes label-keyed:
   `llm_queue_depth{flow_id=...}`.  (Ephemeral flows would explode label
   cardinality — for ephemeral, use label `flow_id="ephemeral"` aggregated.)
6. Tests:
   * header `X-LLM-Flow-ID: agent-1` -> ticket has `agent-1`,
   * no header + body `{ "metadata": {"flow_id":"agent-2"} }` -> ticket has
     `agent-2`,
   * neither -> ticket's id starts with `ephemeral-`,
   * header wins over metadata when both present.

## Verification

* `cargo test --test flow_identify` green.
* A request with `X-LLM-Flow-ID: coding-agent` shows up against a flow with
  `default_weight` / `default_priority` in the registry.
* `llm_queue_depth{flow_id="ephemeral"}` exists; named flows get their own
  label value.
* No card explosion: confirm `flow_id="ephemeral"` aggregates instead of
  per-UUID labels.

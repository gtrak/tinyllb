# 03 — `X-LLM-Priority` header resolution

**Phase:** 2 (client intent)
**Depends on:** `01`.
**Blocks:** `04`, `06`.

## Objective

Parse the `X-LLM-Priority` request header and surface it to the
scheduler so a flow can be explicitly pinned. The header persists
across requests (per-flow state, not per-request) and can be unset
with the literal value `auto`.

## Files

| File | Change |
| --- | --- |
| `src/flow/identify.rs` | Parse the header; return `ResolvedFlow { flow_id, priority_override, unset_override }` from `resolve()`. |
| `src/flow/mod.rs` | Add `PriorityClass` enum and `ResolvedFlow` struct. |
| `src/gateway/proxy.rs` | Read `ResolvedFlow.priority_override` and call `scheduler.apply_priority_override(...)` before `admit()`. |
| `src/scheduler/mod.rs` | Add `apply_priority_override(flow_id, override, unset, classes)` that updates `flow.priority` and `flow.priority_source`. |
| `src/api/flows.rs` | Extend `RegisterFlowResponse` to echo `priority_source` (admin pins as source=2). |
| `tests/priority_header.rs` | NEW: header parse + override tests. |

## Steps

1. Define `PriorityClass` in `src/flow/mod.rs`:

   ```rust
   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   pub enum PriorityClass {
       Interactive,
       Agent,
       Background,
   }
   ```

   Add a `resolve_token(s: &str) -> Option<PriorityClass>` helper
   (case-insensitive match on the literal words; `auto` is handled
   separately below).

2. Change `identify::resolve` to return a richer struct:

   ```rust
   pub struct ResolvedFlow {
       pub flow_id: FlowId,
       pub priority_override: Option<PriorityClass>,
       pub unset_override: bool,   // true when client sent "auto"
   }
   ```

   Keep the existing flow-ID resolution order (`X-LLM-Flow-ID` →
   session headers → body → ephemeral). Add a second pass that reads
   `X-LLM-Priority`:

   - Absent header ⇒ `priority_override = None`, `unset=false`
     (heuristic in effect, no state change).
   - `auto` (case-insensitive) ⇒ also `None`, but `unset_override=true`
     so the scheduler can clear any pin currently set on the flow.
   - `interactive`/`agent`/`background` ⇒ `Some(class)`,
     `unset_override=false`.
   - Unknown value ⇒ log a `WARN` and proceed as if absent (`None`,
     `unset=false`). Don't reject the request.

3. Backward compatibility: existing tests in `tests/flow_identify.rs`
   call `resolve(&headers, &body)` and expect a `FlowId`. Update call
   sites to use `resolve(...).flow_id` — keeps tests focused on
   flow ID resolution while exercising the new return type.

4. In `src/scheduler/mod.rs` add a public helper:

   ```rust
   impl Scheduler {
       pub fn apply_priority_override(
           &self,
           flow_id: &FlowId,
           override_class: Option<PriorityClass>,
           unset: bool,
           classes: &Priorities,
       ) {
           let flow = self.registry.get_or_create(flow_id.clone());
           if unset {
               // Clear previous explicit pin and resume heuristic.
               flow.set_priority_source(0);
               flow.set_priority(classes.agent);  // or current default
               return;
           }
           if let Some(class) = override_class {
               let v = match class {
                   PriorityClass::Interactive => classes.interactive,
                   PriorityClass::Agent => classes.agent,
                   PriorityClass::Background => classes.background,
               };
               flow.set_priority(v);
               flow.set_priority_source(1); // header
           }
           // None && !unset: no header in this request, keep state.
       }
   }
   ```

5. `src/gateway/proxy.rs::proxy_handler`:

   - Replace `let flow_id = identify::resolve(...)` with
     `let resolved = identify::resolve(...)`.
   - Before calling `scheduler.admit(resolved.flow_id.clone(),
     work_unit)`, look up `state.config.priorities` and call
     `state.scheduler.apply_priority_override(...)`.
   - The flag `unset` must be honored even when no prior override
     existed (idempotent reset).

6. Admin API path (`src/api/flows.rs::register_handler`): set
   `priority_source = 2` on the flow when upserting. The existing
   `FlowRegistry::register` already sets `priority`; add a parallel
   write to `priority_source` so the heuristic stays out of the way
   of admin pins. Update `RegisterFlowResponse` to include
   `priority_source: u8` so callers can confirm.

7. Tests in `tests/priority_header.rs`:
   - `explicit_header_pins_flow`: send `X-LLM-Priority: interactive`,
     assert `flow.priority() == 100` and `priority_source() == 1`.
   - `auto_header_unsets_prior_pin`: previously pinned flow,
     subsequent request with `auto` ⇒ `priority_source() == 0`
     and `priority()` returns the `agent` default.
   - `unknown_header_value_ignored`: `X-LLM-Priority: garbage` ⇒
     `priority_override == None` and `unset == false`.
   - `case_insensitive`: `INTERACTIVE` and `Background` parse
     correctly.
   - `x_llm_flow_id_takes_precedence`: combined
     `X-LLM-Flow-ID: foo` + `X-LLM-Priority: background` ⇒ both are
     applied (flow_id is `foo`, override is `background`); flow
     identity precedence is unchanged by the new header.

## Verification

- `cargo test --test priority_header` green.
- `cargo test --test flow_identify` still green (no regressions).
- Manual curl against a dev proxy:
  ```bash
  curl -sS http://localhost:1234/v1/chat/completions \
    -H 'X-LLM-Flow-ID: test-flow' \
    -H 'X-LLM-Priority: interactive' \
    -d '{"model":"...","messages":[...],"stream":true}'
  ```
  followed by a `GET /flows` (if available) or metrics scrape showing
  `llm_flow_priority_class{flow_id="test-flow"} 100` and
  `llm_flow_priority_source{flow_id="test-flow",source="header"} 1`.

## Notes

- Header parsing is the only new client-facing surface in this plan.
  Document it in `docs/plans/001-llm-qdisc-proxy/PRIORITY.md` (issue
  07) once behavior stabilizes.
- The `unset_override` flag is a deliberate choice over hiding the
  reset behind a `POST /flows` admin call. Clients need a way to
  dynamically step down from a high-priority pin without out-of-band
  admin coordination.

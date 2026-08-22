# 02 — Gateway reads live count; remove `llamacpp_slots` config

- **Complexity:** M
- **Timebox:** 60 min
- **Depends on:** 01

## Objective

Switch the source of the slot count from the (now-removed) config field to the
live `BackendSnapshot.slot_count`, and delete the `llamacpp_slots` config knob
end-to-end. Update the pinning tests to inject `slot_count` via a watch channel.

## Files

| File | Change |
|------|--------|
| `src/gateway/mod.rs` | Remove `AppState.llamacpp_slots`; add `snapshot_rx: watch::Receiver<BackendSnapshot>`; `test_default` sets an empty snapshot receiver. |
| `src/gateway/proxy.rs` | `id_slot` computation reads `state.snapshot_rx.borrow().slot_count` instead of `state.llamacpp_slots`. |
| `src/main.rs` | Remove `llamacpp_slots: cfg.backend.llamacpp_slots`; add `snapshot_rx: monitor.snapshot_receiver()`. |
| `src/config/mod.rs` | Delete `Backend.llamacpp_slots` field + its `Default` entry. |
| `src/config/loader.rs` | Delete the `llamacpp_slots` validation block (and any explanatory comment). |
| `config.example.yaml` | Delete the commented `llamacpp_slots` lines. |
| `tests/config.rs` | Delete the three `llamacpp_slots_*` tests. |
| `tests/slot_pinning.rs` | Update all tests to inject `slot_count` via a watch channel instead of setting `llamacpp_slots`. |

## Context (verified facts — do not re-derive)

- `AppState` (`src/gateway/mod.rs:19-40`) already has
  `stall_rx: tokio::sync::watch::Receiver<bool>` — `snapshot_rx` follows the
  exact same pattern. `test_default` (`:48-68`) sets `stall_rx:
  BackendMonitor::empty().stall_receiver()`.
- `BackendMonitor::snapshot_receiver()` (`src/backend/mod.rs:402`) returns a
  cloned `watch::Receiver<BackendSnapshot>`; `BackendMonitor::empty()`'s
  receiver has a snapshot with `slot_count: None` and `changed()` errors
  immediately (we only `borrow()`, never `select` on it, so that's fine).
- Current `id_slot` computation in `proxy_handler` (task 009):
  ```rust
  let is_inference = is_inference_request(&method, &original_path);
  let id_slot: Option<u32> = match (is_inference, flow_id.is_ephemeral(), state.llamacpp_slots) {
      (true, false, Some(n)) => Some(slot_id_for_flow(&flow_id.to_string(), n)),
      _ => None,
  };
  ```
- `BackendSnapshot` is `pub` with `pub` fields and a manual `Default`
  (`slot_count: None` after task 01). Import path in tests:
  `tinyllb::backend::BackendSnapshot` (confirm the crate re-export; plan 008
  tests use `tinyllb::backend::BackendMonitor`, so `BackendSnapshot` is
  reachable the same way).
- `main.rs` builds the real `AppState` and already has the `BackendMonitor`
  in scope (the DRR scheduler receives `monitor.snapshot_receiver()` in
  plan 008) — call it again for `AppState.snapshot_rx`.

## Implementation spec

1. **`src/gateway/mod.rs`**:
   - Remove `pub llamacpp_slots: Option<u32>` (and its doc comment).
   - Add:
     ```rust
     /// Latest backend snapshot (for reading the live llama.cpp slot count
     /// for `id_slot` pinning). `slot_count` is `None` until the first
     /// successful `/slots` scrape (or for vLLM) ⇒ no pinning. See plan 010.
     pub snapshot_rx: tokio::sync::watch::Receiver<BackendSnapshot>,
     ```
     (Import `BackendSnapshot` if not already imported in this file.)
   - In `test_default`, replace the `llamacpp_slots: None` line with
     `snapshot_rx: crate::backend::BackendMonitor::empty().snapshot_receiver()`.

2. **`src/gateway/proxy.rs`** — replace the `id_slot` computation with:
   ```rust
   let is_inference = is_inference_request(&method, &original_path);
   let slot_count = state.snapshot_rx.borrow().slot_count;
   let id_slot: Option<u32> = match (is_inference, flow_id.is_ephemeral(), slot_count) {
       (true, false, Some(n)) if n >= 1 => Some(slot_id_for_flow(&flow_id.to_string(), n)),
       _ => None,
   };
   ```
   Everything else in the body-build / `Content-Length` / retry logic is
   UNCHANGED (it already consumes `id_slot`).

3. **`src/main.rs`** — in the real `AppState` construction: remove
   `llamacpp_slots: cfg.backend.llamacpp_slots,`; add
   `snapshot_rx: monitor.snapshot_receiver(),` (reuse the existing `monitor`
   binding; confirm its name). The in-file `#[cfg(test)]` `AppState` (if any)
   gets `snapshot_rx: crate::backend::BackendMonitor::empty().snapshot_receiver()`.

4. **`src/config/mod.rs`** — delete the `llamacpp_slots` field + doc comment
   from `Backend`, and `llamacpp_slots: None` from its manual `Default`.

5. **`src/config/loader.rs`** — delete the `if let Some(n) = cfg.backend.llamacpp_slots`
   validation block (and any explanatory comment about it from task 009).

6. **`config.example.yaml`** — delete the commented `llamacpp_slots` block.

7. **`tests/config.rs`** — delete `llamacpp_slots_defaults_to_none`,
   `llamacpp_slots_parses_some`, `llamacpp_slots_zero_rejected`.

8. **`tests/slot_pinning.rs`** — rework the harness so each test injects
   `slot_count` via a watch channel and overrides `AppState.snapshot_rx`:
   ```rust
   let (tx, rx) = tokio::sync::watch::channel(BackendSnapshot::default());
   // For "enabled" tests, send a snapshot with the desired count:
   let _ = tx.send(BackendSnapshot { slot_count: Some(4), ..Default::default() });
   let state = AppState { snapshot_rx: rx, ..AppState::test_default(/*...*/) };
   ```
   Keep `tx` alive in the test (so the channel isn't closed). Mapping of the
   existing tests:
   - `named_session_injects_id_slot` — `slot_count: Some(4)` → id_slot
     `== slot_id_for_flow("ses_a", 4)`.
   - `same_session_same_slot` — `slot_count: Some(4)` → two requests, same id_slot.
   - `ephemeral_omits_id_slot` — `slot_count: Some(4)` but NO session header → no id_slot.
   - `disabled_omits_id_slot` — **`slot_count: None`** (default snapshot, don't send
     Some) with a session header → no id_slot AND body byte-identical. (This is
     the new "off" case replacing "config None".)
   - `id_slot_is_integer` — `slot_count: Some(4)` → id_slot is a JSON number.
   - `models_route_never_pinned` — `GET /v1/models` → no id_slot.
   - `id_slot_survives_retry` — `slot_count: Some(4)` → both retry attempts carry
     the same id_slot.
   - The in-file `inject_id_slot` unit tests in proxy.rs are UNCHANGED (they test
     the pure helper, independent of the source of N).

## CRITICAL CONSTRAINTS

- **Pinning behavior is unchanged** — only the *source* of N changes (config →
  snapshot). Named inference requests with `slot_count Some(n)` pin to
  `fnv1a(flow) % n`; ephemeral / non-inference / `slot_count None` never pin;
  `id_slot` is a JSON integer; retries carry it. The `Content-Length`
  drop-if-changed logic is untouched.
- **`slot_count: None` ⇒ no pinning AND byte-identical body** (the off path).
  The `disabled_omits_id_slot` test must assert byte-identity.
- **No changes** to scheduling, admission, KV gate, retry counting, backoff, or
  token accounting. The only behavioral change is where N comes from.
- Do NOT touch `src/flow/mod.rs` (`slot_id_for_flow` is unchanged) or
  `src/backend/mod.rs` (task 01 done — only consume `slot_count`).
- No `// @lat:` comments (docs land in task 03).

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
lat check
```

**Regression gates (critical):**
- All plan 008 KV-pressure tests pass unchanged.
- All plan 009 pinning behavior holds — the 7 `slot_pinning` tests (reworked to
  inject via snapshot) pass; the in-file `inject_id_slot` unit tests pass.
- Config tests: the 3 `llamacpp_slots_*` tests are gone; the remaining config
  tests still pass (and a config WITHOUT `llamacpp_slots` — now that the field
  is removed — still loads fine).
- `lat check` = "All checks passed".

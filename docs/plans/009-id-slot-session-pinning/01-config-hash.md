# 01 — Config `llamacpp_slots` + `slot_id_for_flow` hash

- **Complexity:** S
- **Timebox:** 40 min
- **Depends on:** none

## Objective

Add the opt-in config knob and the pure deterministic hash that maps a flow id
to a llama.cpp slot index. No request-path changes yet (that's task 02).

## Files

| File | Change |
|------|--------|
| `src/config/mod.rs` | `Backend.llamacpp_slots: Option<u32>` (serde default `None`) + `Default` impl. |
| `src/config/loader.rs` | default `None`; validation: if `Some(n)`, require `n >= 1`. |
| `src/flow/mod.rs` | `pub fn slot_id_for_flow(flow: &str, slot_count: u32) -> u32` (FNV-1a) + in-file unit tests. |
| `config.example.yaml` | commented `llamacpp_slots` example under `backend:`. |
| `tests/config.rs` | config parse/validation tests. |

## Context (verified facts — do not re-derive)

- `Backend` struct: `src/config/mod.rs:46-67`. It already has
  `kv_unified: bool` with `#[serde(default)]` (line 65-66) and a manual
  `impl Default` (line 69-79). Mirror that exact style.
- Loader defaults live in `src/config/loader.rs` (the `set_default(...)` group
  used by `kv_unified` / `kv_pressure` in plan 008). `validate()` is where
  config errors are raised (see the `kv_pressure` validation block added in
  plan 008 task 02 for the error-message style).
- `FlowId` is `pub struct FlowId(String)` (`src/flow/mod.rs:48`); the hash takes
  the plain `&str` (the gateway will pass `&flow_id.to_string()`), NOT a
  `FlowId` — keep the function dependency-free and trivially testable.
- No hash crate is present (only `serde_json`, `uuid`). Implement FNV-1a inline.

## Implementation spec

1. `src/config/mod.rs`:
   ```rust
   /// llama-server slot count for `id_slot` session pinning. Mirrors the
   /// llama-server `--parallel N` flag. `None` (default) disables pinning;
   /// `Some(n)` pins each named session to slot `fnv1a(flow_id) % n`.
   /// Ignored for vLLM backends.
   #[serde(default)]
   pub llamacpp_slots: Option<u32>,
   ```
   Add `llamacpp_slots: None` to the manual `Default` impl.

2. `src/config/loader.rs`:
   - `set_default("backend.llamacpp_slots", serde_json::Value::Null)` (or
     however `Option` defaults are expressed for the other fields — match the
     existing `Option`/default idiom in the loader; if no `Option` default
     precedent exists, rely on `#[serde(default)]` and add a comment).
   - In `validate()`: `if let Some(n) = cfg.backend.llamacpp_slots { if n == 0 {
     return Err(... "backend.llamacpp_slots must be >= 1 when set") } }`.

3. `src/flow/mod.rs` — pure hash (place near the top, before or after the
   `FlowId` impl; keep it a free `pub fn`):
   ```rust
   /// Deterministic llama.cpp slot index for a flow id (FNV-1a 64-bit mod
   /// slot_count). Stable across restarts (unlike the randomized HashMap
   /// hasher) so a session keeps the same slot and its KV cache.
   /// `slot_count == 0` → 0 (defensive; config validation forbids it).
   pub fn slot_id_for_flow(flow: &str, slot_count: u32) -> u32 {
       if slot_count == 0 {
           return 0;
       }
       const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
       const PRIME: u64 = 0x0000_0100_0000_01b3;
       let mut h: u64 = OFFSET;
       for b in flow.as_bytes() {
           h ^= *b as u64;
           h = h.wrapping_mul(PRIME);
       }
       (h % slot_count as u64) as u32
   }
   ```

4. `config.example.yaml` — under `backend:`, after `kv_unified`:
   ```yaml
   # llama.cpp only: slot count for id_session pinning (mirror --parallel).
   # None/unset disables; set to e.g. 4 to pin named sessions to a stable slot.
   # llamacpp_slots: 4
   ```

## Tests

`src/flow/mod.rs` in-file `#[cfg(test)]` (add a `mod tests` if none exists there
for this function — check first; flow/mod.rs may already have a test module):
- `slot_id_deterministic` — same input, same output (call twice, equal).
- `slot_id_in_range` — for `n` in {1,2,4,7,1000}: `slot_id_for_flow("ses_a", n) < n`.
- `slot_id_n1_is_zero` — `slot_id_for_flow(anything, 1) == 0`.
- `slot_id_zero_count_is_zero` — `slot_id_for_flow("x", 0) == 0`.
- `slot_id_distributes` — 1000 distinct flow ids into `n=8` populate more than
  one bucket (assert at least, say, 4 distinct slots used) — guards against a
  degenerate constant hash.

`tests/config.rs`:
- `llamacpp_slots_defaults_to_none` — minimal config → `None`.
- `llamacpp_slots_parses_some` — `backend.llamacpp_slots: 4` → `Some(4)`.
- `llamacpp_slots_zero_rejected` — `backend.llamacpp_slots: 0` → load/validate
  error containing "llamacpp_slots".

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
lat check
```

`lat check` may report a NEW dangling ref only if you add a `// @lat:` comment
in this task — do NOT add `// @lat:` comments here (docs/section land in task
03). `lat check` should stay clean (it is currently "All checks passed").

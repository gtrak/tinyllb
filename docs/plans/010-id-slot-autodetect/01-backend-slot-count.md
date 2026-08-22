# 01 — Backend: surface `slot_count` in `BackendSnapshot`

- **Complexity:** S
- **Timebox:** 35 min
- **Depends on:** none

## Objective

Stop discarding the `/slots` count. Refactor the llama.cpp `/slots` fetch to
return the parsed slots, and surface `slot_count` (plus the existing `kv_usage`)
in the published `BackendSnapshot`. **No gateway/config changes yet** (task 02).

## Files

| File | Change |
|------|--------|
| `src/backend/mod.rs` | Add `BackendSnapshot.slot_count: Option<u32>` (+ Default); refactor `fetch_slots_kv_usage` → `fetch_slots` returning `Option<Vec<SlotKv>>`; poll loop sets `slot_count` from the parse; preserve the warn-once state machine. |

## Context (verified facts — do not re-derive)

- `parse_slots(body) -> Option<Vec<SlotKv>>` at `src/backend/mod.rs:307`.
- `slots_kv_usage(slots, kv_unified) -> Option<f64>` at `:327` (pure; keep as-is).
- `fetch_slots_kv_usage(client, slots_url, kv_unified) -> Option<f64>` at
  `:456-468` — fetches `/slots`, checks status, `parse_slots`, then
  `slots_kv_usage`. **This is what you refactor.**
- Poll loop `/slots` block at `:532-557`: `if result.found_llamacpp { ... }`,
  with the `last_slots_ok: Option<bool>` warn-once state machine
  (good→bad warns, bad→good infos, never-working silent).
- `BackendSnapshot` struct at `:101-124`; manual `impl Default` at `:126-140`.

## Implementation spec

1. **`BackendSnapshot`** — add a field (after `kv_free`, or in a sensible spot):
   ```rust
   /// Number of llama.cpp slots the server reported this scrape
   /// (`/slots` length). `None` when `/slots` was unavailable for this scrape
   /// (cold start, or a backend that doesn't expose it, e.g. vLLM). Used for
   /// `id_slot` session pinning.
   pub slot_count: Option<u32>,
   ```
   Add `slot_count: None` to the manual `Default` impl (`:126-140`).

2. **Refactor the fetch helper** — replace `fetch_slots_kv_usage` with:
   ```rust
   /// Fetch and parse the llama-server `/slots` endpoint. Returns the parsed
   /// slots, or `None` on any failure (HTTP error, non-success status,
   /// malformed JSON, empty array). The caller derives both `kv_usage` and
   /// `slot_count` from the result.
   async fn fetch_slots(client: &reqwest::Client, slots_url: &Url) -> Option<Vec<SlotKv>> {
       let response = client.get(slots_url.clone()).send().await.ok()?;
       if !response.status().is_success() {
           return None;
       }
       let body = response.text().await.ok()?;
       parse_slots(&body)
   }
   ```
   (Delete `fetch_slots_kv_usage` — it has one caller, the poll loop.)

3. **Poll loop** — replace the `if result.found_llamacpp { ... }` block's
   `/slots` handling so it fetches once, derives both values, and preserves the
   warn-once machine:
   ```rust
   if result.found_llamacpp {
       let slots = Self::fetch_slots(&client, &slots_url).await;
       let ok = slots.is_some();
       match slots {
           Some(slots) => {
               result.snapshot.slot_count = Some(slots.len() as u32);
               if let Some(kv_usage) = slots_kv_usage(&slots, kv_unified) {
                   result.snapshot.kv_usage = kv_usage;
                   result.snapshot.kv_free = 1.0 - kv_usage;
               }
           }
           None => {
               result.snapshot.slot_count = None;
               // kv_usage stays at its 0.0 default for this scrape.
           }
       }
       // Preserve the EXISTING warn-once state machine on `ok` (the
       // last_slots_ok good→bad / bad→good / never-working logic), unchanged.
       match (last_slots_ok, ok) { /* ...existing body... */ }
   }
   ```
   **NOTE on `ok` semantics:** plan 008 derived `ok = usage.is_some()` (i.e.
   `slots_kv_usage` succeeded). The spec above uses `ok = slots.is_some()`
   (fetch+parse succeeded). These differ only in the rare zero-pool edge case
   (slots parsed but `kv_usage` underivable). Prefer `ok = slots.is_some()`
   (the endpoint genuinely worked) — but **if this breaks a plan 008 test**,
   fall back to `ok = slots_kv_usage(&slots, kv_unified).is_some()` and report
   it. Run the full suite to confirm.

4. **`slot_count` is set only for llama.cpp.** The block is already gated on
   `result.found_llamacpp`, so vLLM scrapes leave `slot_count` at its default
   `None`. Good — do not set it for vLLM.

## Tests

Update/add in `src/backend/mod.rs` in-file tests (the plan 008 `/slots` tests
live there):
- `parse_slots` + `slots_kv_usage` tests: unchanged (still pass).
- Add a test that the poll-loop path surfaces `slot_count`: if there's an
  existing helper/test that drives `fetch_slots` or the `/slots` block with a
  stub, extend it to assert `snapshot.slot_count == Some(N)` for an N-slot body
  and `None` on a fetch failure. If no such harness exists (the `/slots` fetch
  is async and needs a stub server), add a focused unit test on `fetch_slots`
  using a minimal axum stub that returns a known `/slots` JSON, asserting the
  returned `Vec` length (which is what becomes `slot_count`). Keep it minimal.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
lat check
```

**Regression gate (critical):** all plan 008 `/slots` KV-pressure tests
(`parse_slots_*`, `slots_kv_usage_*`, and any warn-once tests) must still pass
unchanged. `kv_usage`/`kv_free` derivation must be byte-identical to before
(same `slots_kv_usage` call). `lat check` stays "All checks passed" (add no
`// @lat:` refs; docs land in task 03).

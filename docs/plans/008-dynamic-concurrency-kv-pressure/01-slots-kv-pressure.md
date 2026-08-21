# 01 — /slots KV-pressure derivation (backend monitor)

- **Complexity:** M
- **Timebox:** 60 min
- **Depends on:** nothing (foundational)

## Objective

Make the backend monitor derive a live KV-usage signal for llama.cpp
backends from llama-server's `/slots` endpoint, writing it into
`BackendSnapshot.kv_usage` so that `KvPolicy`, `KvBias`, and (task 03) the
dynamic cap all see real pressure. vLLM behavior must be byte-identical.

## Files

| File | Change |
|------|--------|
| `src/backend/mod.rs` | Slots JSON parser; poll-loop `/slots` fetch + derived `kv_usage`; warn-once on good→bad transition; write `llm_backend_kv_pressure` gauge on publish. |
| `src/config/mod.rs` | `Backend.kv_unified: bool` (default `false`), documented as mirroring llama-server's `-kvu` flag. |
| `src/config/loader.rs` | `set_default("backend.kv_unified", false)`. No validation needed for a bool. |
| `src/metrics/mod.rs` | `llm_backend_kv_pressure` gauge (help: "Latest KV usage fraction from the backend snapshot (vLLM gauge or llama.cpp /slots-derived)"). |
| `config.example.yaml` | `backend.kv_unified: false` with a comment (only when set does the example change; keep it minimal). |
| `src/backend/mod.rs` (tests) | In-file unit tests for the parser and pressure computation. |

`lat.md` is task 05.

## Context (verified facts — do not re-derive)

- llama-server `/slots` requires `--slots` (default off). Response is a JSON
  **array** of slot objects. Verified field shape (llama.cpp
  `tools/server/server-context.cpp`, `server_slot::to_json`):
  ```json
  [
    {
      "id": 0,
      "n_ctx": 340000,
      "speculative": false,
      "is_processing": true,
      "id_task": 123,
      "n_prompt_tokens": 168000,
      "n_prompt_tokens_processed": 168000,
      "n_prompt_tokens_cache": 0,
      "params": { ... },
      "next_token": { "has_next_token": true, "has_new_line": false, "n_remain": 500, "n_decoded": 100 }
    },
    { "id": 1, "n_ctx": 340000, "speculative": false, "is_processing": false }
  ]
  ```
  `n_prompt_tokens` is `prompt.tokens.size()` — present only when the slot
  has a current or previous task. It equals the slot's **resident** token
  count: idle slots keep reporting it after completion (KV stays resident)
  and it reads 0 after the server reclaims the KV. So Σ `n_prompt_tokens`
  over all slots == resident KV in the pool.
- Default response omits `prompt`/`generated` text fields (small bodies).
- **`-kvu` (unified KV):** pool = single `n_ctx` shared by all slots, and
  **every slot reports `n_ctx` = the full pool size**. Utilization =
  Σ tokens ÷ `n_ctx` (take any slot's value). Without `-kvu`, pool =
  Σ `n_ctx` over slots.
- `llamacpp:n_tokens_max` in `/metrics` is a monotonic high-watermark — do
  NOT use it.
- The poll loop already self-identifies flavor per scrape
  (`found_llamacpp` flag from plan 007). `/slots` fetch happens only when
  the scrape's flavor is llama.cpp.
- The monitor already builds the metrics URL from the base URL
  (`Self::metrics_url`); add an analogous `slots_url`.

## Steps

1. **Config:** add `pub kv_unified: bool` to `Backend` with
   `#[serde(default)]` and a doc comment ("Whether the llama-server backend
   runs with `-kvu` (unified KV cache). Mirrors the llama-server flag;
   selects the /slots pressure denominator. Ignored for vLLM backends.").
   Add the loader default.
2. **Parser** in `src/backend/mod.rs`:
   ```rust
   /// One slot's resident-KV contribution parsed from `/slots`.
   struct SlotKv { n_ctx: u64, n_prompt_tokens: u64 }

   /// Parse a llama-server `/slots` JSON body into per-slot KV counts.
   /// Returns `None` for malformed JSON, a non-array body, or an empty
   /// array. Slots missing `n_ctx` are skipped; a missing
   /// `n_prompt_tokens` counts as 0. Unknown fields are ignored.
   pub fn parse_slots(body: &str) -> Option<Vec<SlotKv>>
   ```
   Use `serde_json` (already a dependency) with a small local struct deriving
   `Deserialize` that has `n_ctx: u64` and
   `#[serde(default)] n_prompt_tokens: u64`. Note: a slot object missing
   `n_ctx` must not fail the whole parse — either make `n_ctx`
   `Option<u64>` and filter, or parse into a permissive struct.
3. **Pressure computation:**
   ```rust
   /// Derive pool utilization [0,1] from /slots data. `kv_unified` mirrors
   /// llama-server's `-kvu`: pool is the single slot n_ctx, not the sum.
   fn slots_kv_usage(slots: &[SlotKv], kv_unified: bool) -> Option<f64>
   ```
   - `kv_unified`: `used / n_ctx` where `n_ctx` = any slot's value (they are
     identical); guard `n_ctx == 0` → `None`.
   - non-unified: `used / sum(n_ctx)`; guard sum 0 → `None`.
   - clamp result to [0,1].
4. **Poll loop** (`poll_loop`): after a successful `/metrics` parse,
   `if result.found_llamacpp { ... }` — fetch `slots_url` with the existing
   `client`. On success + parse success, set `snapshot.kv_usage` (and
   `kv_free = 1.0 - kv_usage`) from `slots_kv_usage`. On any failure
   (HTTP error, bad JSON, empty, pool 0), leave `kv_usage` at its 0.0
   default for this scrape. Track a `slots_ok: bool` (or
   `Option<&'static str> last_slots_state`) so we `tracing::warn!` once
   when a previously-working `/slots` fetch stops working (and a
   `tracing::info!` when it recovers). Do NOT warn while it has never
   worked (the common case: `--slots` not enabled).
   - Keep the existing publish gate (`found_usage || found_llamacpp`) and
     gauge writes exactly as they are.
5. **Gauge:** in the publish block (both flavors), set
   `metrics.llm_backend_kv_pressure` to the snapshot's `kv_usage`. Register
   it in `create_metrics` next to `vllm_kv_cache_usage`.
6. **Tests** (in-file `mod tests`):
   - `parse_slots_realistic_kvunified`: 4-slot body as in the verified shape
     (one busy with 168000 tokens, n_ctx 340000; three idle, two with
     residual `n_prompt_tokens`, one without) → 4 `SlotKv` entries with the
     right values.
   - `parse_slots_empty_array` → `None`; `parse_slots_malformed_json` →
     `None`; `parse_slots_non_array` → `None`.
   - `parse_slots_missing_n_prompt_tokens_defaults_zero`.
   - `parse_slots_skips_slot_missing_n_ctx`.
   - `slots_kv_usage_kvunified`: 168000/340000 ≈ 0.494 (assert with
     tolerance 1e-9); and the issue's 93% case: 168000/180000 ≈ 0.9333.
   - `slots_kv_usage_nonunified`: two slots n_ctx 1000, tokens 600+200 →
     0.8.
   - `slots_kv_usage_zero_pool` → `None`.
   - `slots_kv_usage_clamped`: tokens > pool → 1.0.
   - Keep all existing tests passing unchanged.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```

All must pass. No behavior change for vLLM backends or for llama.cpp
backends without `/slots` (kv_usage stays 0.0 there).

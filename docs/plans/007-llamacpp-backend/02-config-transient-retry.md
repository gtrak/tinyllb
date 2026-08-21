# 02 — Config: backend.transient_retry

- **Complexity:** XS
- **Timebox:** 20 min
- **Depends on:** nothing

## Objective

Add a `backend.transient_retry` config section that controls the proxy-side
re-forward of transient llama.cpp context errors (and any future transient
backend error), with sensible defaults and validation.

## Files

| File | Change |
|------|--------|
| `src/config/mod.rs` | Add `TransientRetry` struct + `Default` impl; nest it under `Backend` as `pub transient_retry: TransientRetry`. |
| `src/config/loader.rs` | Add env overrides (`TINYLLB__BACKEND__TRANSIENT_RETRY__*`) following the existing `retry_policy` precedent (~loader.rs:188); add validation (only when `max_attempts > 0`). |
| `config.example.yaml` | Add a commented `transient_retry` block under `backend:`. |

## Context

- The existing top-level `retry_policy` (premature-stop, plan 005) is the
  precedent for opt-in retry config with env overrides and validation. Read
  `src/config/loader.rs` around the `retry_policy` handling (~:188) and
  mirror it.
- `backend` already carries `url`, `metrics_interval`, `stall_timeout`;
  `transient_retry` nests cleanly under it.

## Steps

1. In `src/config/mod.rs`, add:
   ```rust
   /// Proxy-side re-forward of transient backend errors (llama.cpp
   /// context-exceed where prompt fits slot capacity but no room yet;
   /// mid-stream KV-exhaustion before any content is forwarded). Bounded
   /// exponential backoff. `max_attempts: 0` disables (zero behavioral
   /// change). See plan 007.
   #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
   pub struct TransientRetry {
       #[serde(default = "TransientRetry::default_max_attempts")]
       pub max_attempts: u32,        // 3
       #[serde(default = "TransientRetry::default_backoff_start", with = "loader::humantime_serde")]
       pub backoff_start: Duration,  // 500ms
       #[serde(default = "TransientRetry::default_backoff_max", with = "loader::humantime_serde")]
       pub backoff_max: Duration,    // 4s
   }
   ```
   with the obvious `default_*` functions and a `Default` impl
   (`max_attempts: 3`, `backoff_start: 500ms`, `backoff_max: 4s`).
2. Add `#[serde(default)] pub transient_retry: TransientRetry,` to the
   `Backend` struct, and `transient_retry: TransientRetry::default()` to
   `Backend::default()`.
3. In `src/config/loader.rs`, add env overrides for the three fields
   (uppercase variants of the path) exactly as `retry_policy` does. Add
   validation in the existing validation hook: when `max_attempts > 0`,
   require `backoff_start > 0` and `backoff_max >= backoff_start`; on
   violation, return a config-load error with a helpful message.
4. In `config.example.yaml`, under the existing `backend:` block, add:
   ```yaml
     # Proxy-side re-forward of transient llama.cpp context errors
     # (intake 400 where prompt fits the slot, or mid-stream KV exhaustion
     # before any content is forwarded). Bounded exponential backoff.
     # max_attempts: 0 disables (zero behavioral change).
     transient_retry:
       max_attempts: 3
       backoff_start: 500ms
       backoff_max: 4s
   ```
   Match the file's existing comment style (note the file currently says
   "how often to poll vLLM /metrics" — that comment can stay; llama.cpp is
   documented in task 07).

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```
Add or extend a quick config test in `tests/config.rs` or `tests/retry_config.rs`
mirroring the premature-stop config test: assert defaults parse; assert an
invalid `backoff_max < backoff_start` is rejected; assert env override
works. (If a new test file is cleaner, add `tests/transient_retry_config.rs`.)

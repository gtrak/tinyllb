# 06 — Tests: backend unit + gateway stub

- **Complexity:** S
- **Timebox:** 40 min
- **Depends on:** 01, 04, 05 (the code under test)

## Objective

Consolidate the test coverage for the llama.cpp backend support: ensure
the backend parser unit tests (some added in 01) are complete and that the
gateway stub tests (some added in 04/05) form a coherent suite. This task
fills gaps and runs the whole suite green.

## Files

| File | Change |
|------|--------|
| `src/backend/mod.rs` | Ensure the `#[cfg(test)] mod tests` block has the four llamacpp tests specified in task 01 (realistic body, mixed-family precedence, no-kv-metric, watchdog-progress). Add any missing ones. |
| `tests/transient_retry.rs` (or `tests/transient_retry_stream.rs`) | Ensure the gateway stub tests from tasks 04 + 05 are present and green: permanent passthrough, transient→success, transient exhaustion, disabled, streaming intake transient, mid-stream transient no-content→success, mid-stream transient after-content→abort, mid-stream permanent. |
| `tests/config.rs` or `tests/transient_retry_config.rs` | Ensure the config test from task 02 (defaults, invalid backoff rejected, env override) is present and green. |

## Context

- The repo's test conventions: unit tests inline in `#[cfg(test)] mod
  tests` (see `src/backend/mod.rs` existing parser tests), and integration
  tests as files in `tests/` (see `tests/gateway.rs`, `tests/premature_stop_retry.rs`
  for the stub-backend + AppState harness pattern).
- `benches/stub_backend.rs` shows a stub backend that returns SSE streams;
  the premature-stop retry tests show how to drive the streaming retry
  path with a stub — mirror that for the transient streaming tests.

## Steps

1. Audit the tests added inline during 01/04/05 against the lists above.
2. Fill any gaps. Prefer inline unit tests for the parser and
   `tests/*.rs` integration tests for the gateway, mirroring the
   premature-stop test file structure.
3. Add one regression test: a vLLM-flavored 4xx error (e.g.
   `{"error":{"type":"some_vllm_error"}}`) is classified `NotLlamacpp` and
   passed through with zero retries — proves the feature doesn't affect
   vLLM error handling.
4. Run the full suite.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```
All green. No flaky tests. The disabled-defaults cases prove zero
behavioral change when the feature is off.

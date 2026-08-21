# 07 — Docs + lat.md

- **Complexity:** S
- **Timebox:** 40 min
- **Depends on:** 01–06 (docs reflect final behavior)

## Objective

Update user-facing docs and the `lat.md` knowledge graph to reflect
llama.cpp backend support: dual-family metrics parsing, the
flavor-aware publish gate, the new snapshot fields, the transient re-forward
concept, the new config, and the new metric counters. Then run `lat check`.

## Files

| File | Change |
|------|--------|
| `README.md` | Add a llama.cpp quickstart (llama-server with `--metrics`; note that `kv_policy`/`kv_bias` are inert without a KV metric); note the `--parallel N` ↔ `max_active_flows` alignment recommendation. |
| `config.example.yaml` | (Done in 02 — verify the `transient_retry` block is present and commented.) |
| `lat.md/backend.md` | Update "vLLM Metrics Parsing" → generalize to dual-family (llama.cpp + vLLM); document per-scrape self-identification, the `found_llamacpp` flag, the `cached_prompt_tokens`/`decode_calls` snapshot fields, and the flavor-aware publish gate (`found_usage \|\| found_llamacpp`). Update invariants/constraints. Add a short note that kv_usage/kv_free stay default on llama.cpp. |
| `lat.md/backend.md` (Stall Watchdog) | Note the extended progress check (four counters) and why (cache-heavy workloads; `prompt_tokens_total` excludes cached tokens). |
| `lat.md/gateway.md` (or `admission.md`) | Add a "Transient Backend-Error Re-forward" concept: intake `exceed_context_size_error` permanent vs transient classification, bounded re-forward, mid-stream no-content re-forward, permanent passthrough. Source link to `[[backend]]`. |
| `lat.md/metrics.md` | Add the `tinyllb_backend_retries_total` + `tinyllb_backend_retry_exhausted_total` counters to the backend-stall family (or a new retry sub-family). |
| `lat.md/config.md` | Document `backend.transient_retry` (max_attempts, backoff_start, backoff_max; 0 disables). |
| `docs/plans/README.md` | Add the 007 row to the plans table. |

## Steps

1. Read the current `lat.md/backend.md`, `lat.md/metrics.md`,
   `lat.md/config.md`, and `lat.md/gateway.md` (or `admission.md`) to match
   their section style (leading paragraph ≤250 chars, then detail).
2. Update the metrics-parsing concept to dual-family. Keep the section id
   stable if other sections link to it (rename heading text, keep the
   `# vLLM Metrics Parsing` heading or generalize it — but check
   `[[...]]` inbound refs first with `lat refs`).
3. Add the transient re-forward concept as a new section under `gateway.md`
   (or `admission.md` if it reads better there). Give it a stable id and
   link it from the stall-watchdog and metrics sections.
4. Update `README.md` quickstart: add a llama-server example block. The
   vLLM example can stay first.
5. Add the 007 row to `docs/plans/README.md`.
6. Run `lat check` and fix any broken wiki links or code refs introduced.

## Context (lat.md rules from AGENTS.md)

- Every section must have a leading paragraph (≤250 chars, excluding
  `[[wiki link]]` content). No empty sections.
- Source code links use full paths, e.g.
  `[[src/backend/mod.rs#parse_snapshot]]`. `lat check` validates they
  exist.
- One `// @lat:` comment per spec section, placed at the relevant code.
  Tasks 01/04/05 should have added `// @lat:` comments at the new code
  (e.g. `// @lat: [[backend#llama.cpp Metrics Parsing]]`); if not, add
  them here. Do not duplicate refs.
- Run `lat search` / `lat refs` to find inbound links before renaming any
  section id.

## Verification

```bash
lat check
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
```
`lat check` must print "All checks passed". No clippy/build/test
regressions from doc/comment changes.

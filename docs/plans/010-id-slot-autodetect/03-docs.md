# 03 — Docs: auto-detect in lat.md, README, config

- **Complexity:** S
- **Timebox:** 35 min
- **Depends on:** 01, 02

## Objective

Bring `lat.md`, `README.md`, and `config.example.yaml` in line with the
auto-detect design: the slot count now comes from `/slots` (via
`BackendSnapshot.slot_count`), and the `backend.llamacpp_slots` config key no
longer exists. Pass `lat check`.

## Files

| File | Change |
|------|--------|
| `lat.md/gateway.md` | Update `# Session Slot Pinning`: N is auto-detected from `/slots` (not a config key); interface reads `BackendSnapshot.slot_count`; add cold-start + graceful-off constraints; drop the `--parallel`-mirroring guidance. |
| `lat.md/config.md` | REMOVE the `backend.llamacpp_slots` documentation (field deleted in task 02). |
| `lat.md/backend.md` | Note the monitor now also surfaces `slot_count` in the snapshot (from the same `/slots` scrape that derives `kv_usage`). |
| `README.md` | llama.cpp quickstart: remove the `backend.llamacpp_slots: N` line; replace with "session pinning is automatic — the slot count is read from the server's `/slots` endpoint (the same one used for KV pressure); no config needed; disabled if `/slots` is unavailable." |
| `config.example.yaml` | (Verify task 02 deleted the commented `llamacpp_slots` block; adjust only if a stray line remains.) |

## What to do

1. **`lat.md/gateway.md` `# Session Slot Pinning`** — revise to reflect
   auto-detection. Key corrections vs the current (plan 009) text:
   - Interface: the slot count is `BackendSnapshot.slot_count`, read live from
     the backend monitor's snapshot (`AppState.snapshot_rx`), NOT a config key.
     `slot_count = Some(n)` when the last `/slots` scrape reported n slots,
     `None` otherwise. Pin = `slot_id_for_flow(flow, n) = fnv1a(flow) % n`
     (unchanged). `id_slot` injected as a JSON integer for named inference
     requests only (unchanged).
   - Constraints: add "cold start — `slot_count` is `None` until the first
     successful `/slots` scrape (≈ one `metrics_interval`), so the first
     requests auto-select"; "graceful off — if `/slots` is unavailable
     (or vLLM), `slot_count` stays `None` and pinning is disabled". REMOVE the
     "n should mirror `--parallel`" and "out-of-range wraps" config guidance
     (no longer config-driven; N is always the real count). Keep the
     deterministic-hash constraint (why the randomized hasher is wrong).
   - Rationale: keep "KV reuse → lower TTFT" and "hash over free-list"; UPDATE
     the "config over /slots auto-detect" point — it's now the OPPOSITE: we
     auto-detect from `/slots` (which the monitor already polls for KV
     pressure), so there's no config to drift from `--parallel` and N is always
     correct. Note it reuses the existing `/slots` scrape (no new polling).
   - Invariants: keep all; adjust "disabled/None ⇒ byte-identical" to
     "`slot_count: None` (or vLLM) ⇒ no id_slot ⇒ byte-identical".
   - Update the `Related` code-ref if needed: the `// @lat:` anchor on
     `slot_id_for_flow` (src/flow/mod.rs) still resolves; the section id stays
     exactly `Session Slot Pinning`.
2. **`lat.md/config.md`** — remove the `backend.llamacpp_slots` bullet + its
   validation-boundary line added in plan 009. Confirm no other config doc
   references it.
3. **`lat.md/backend.md`** — in the KV-Cache Monitor / snapshot section, note
   the snapshot now carries `slot_count: Option<u32>` (the `/slots` length),
   surfaced from the same scrape that derives `kv_usage`; `None` when
   `/slots` is unavailable. Keep it consistent with the existing
   `kv_usage`-from-`/slots` text.
4. **`README.md`** — llama.cpp quickstart: replace the `backend.llamacpp_slots`
   line with the automatic-pinning note (see table).
5. **`config.example.yaml`** — confirm no `llamacpp_slots` line remains (task 02
   deleted it). Adjust only if a stray line/comment remains.
6. Run `lat check` — fix every error. Must end "All checks passed".

## Verification

```bash
lat check                 # must be "All checks passed"
cargo test --all          # must still pass (docs-only; no code change expected)
cargo clippy --all-targets -- -D warnings
```

`cargo test --all` and clippy should be untouched (this task is docs-only). If a
`// @lat:` ref needs a touch, it must be comment-only and must keep `lat check`
green.

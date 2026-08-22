# Plan 010 — id_slot Auto-Detect (supersedes plan 009's config knob)

Simplify plan 009: instead of a `backend.llamacpp_slots` config field the
operator must keep in sync with `--parallel`, the proxy **auto-detects the slot
count** from the llama-server `/slots` endpoint — which it *already polls and
parses every second* for the KV-pressure feature (plan 008). We just stop
throwing the slot count away.

## Why this is simpler

Plan 008's monitor poll loop (`src/backend/mod.rs`) already:
1. Fetches `/slots` every `metrics_interval` (1s) for llama.cpp scrapes.
2. Parses it into `Vec<SlotKv>` (`parse_slots`, `backend/mod.rs:307`).
3. Reduces it to a single `kv_usage` scalar — **discarding the slot count and
   per-slot ids.**

So the data needed to pick a slot is already in flight. This plan surfaces the
count and removes the config field. No new polling, no new endpoint, no new
flag.

## Change vs plan 009

| Plan 009 (shipped) | Plan 010 (this) |
|--------------------|-----------------|
| `backend.llamacpp_slots: Option<u32>` config (mirror `--parallel`) | **Removed.** |
| `AppState.llamacpp_slots` (static) | **`AppState.snapshot_rx`** (live `watch::Receiver<BackendSnapshot>`, same pattern as existing `stall_rx`). |
| Pin to `fnv1a(flow) % config_n` | Pin to `fnv1a(flow) % snapshot.slot_count`. |
| Operator sets + maintains a config value | Zero config; N always equals the server's real slot count. |

The `slot_id_for_flow` FNV-1a hash, the "named inference requests only" gate,
the `id_slot` integer injection, and the retry-carry behavior are **unchanged**.

## Verified llama.cpp facts (do not re-derive)

- `GET /slots` is registered unconditionally (`tools/server/server.cpp:273`).
  The `routes.get_slots = models_routes->proxy_get` alias (`server.cpp:220`) is
  the multi-backend **router** mode only; a normal single-server returns the
  real slot array. Plan 008 already consumes this endpoint successfully.
- The slot array entries carry `id`, `n_ctx`, `n_prompt_tokens`. Slot ids are
  0-indexed, contiguous 0..N-1; `get_slot_by_id` wraps `id_slot % slots.size()`,
  so mapping a flow into `[0, N)` is safe.
- `BackendSnapshot` has a manual `Default` (`backend/mod.rs:126`) — add
  `slot_count: None` to it.

## Design

### Backend — surface the count
- `fetch_slots_kv_usage(client, url, kv_unified) -> Option<f64>` becomes
  `fetch_slots(client, url) -> Option<Vec<SlotKv>>` (fetch + parse; no
  reduction). The poll loop computes `kv_usage` via `slots_kv_usage` AND sets
  `snapshot.slot_count = Some(slots.len() as u32)` from the same parse.
- `BackendSnapshot` gains `pub slot_count: Option<u32>` (None = `/slots`
  unavailable this scrape, e.g. cold start or server without slot info).
- **Preserve plan 008's warn-once `/slots` health state machine** (the
  `last_slots_ok` good→bad / bad→good logic) — base its `ok` on "did `/slots`
  fetch+parse succeed" (`fetch_slots` returned `Some`). If this changes any
  plan 008 test's expectation, report it (do not silently alter the semantics).

### Gateway — read the live count, drop the config
- `AppState`: remove `llamacpp_slots: Option<u32>`; add
  `snapshot_rx: tokio::sync::watch::Receiver<BackendSnapshot>`.
  `test_default` sets `snapshot_rx: BackendMonitor::empty().snapshot_receiver()`
  (its snapshot has `slot_count: None` ⇒ no pinning by default in tests).
- `proxy_handler`: the `id_slot` computation reads
  `state.snapshot_rx.borrow().slot_count` instead of `state.llamacpp_slots`:
  ```rust
  let id_slot: Option<u32> = match (is_inference, flow_id.is_ephemeral(),
                                    state.snapshot_rx.borrow().slot_count) {
      (true, false, Some(n)) if n >= 1 => Some(slot_id_for_flow(&flow_id.to_string(), n)),
      _ => None,
  };
  ```
  `slot_id_for_flow` already guards `n == 0 → 0`, but the `n >= 1` guard makes
  "no usable slots ⇒ no pinning" explicit (avoids pinning everything to slot 0).
- `main.rs`: remove `llamacpp_slots: cfg.backend.llamacpp_slots`; add
  `snapshot_rx: monitor.snapshot_receiver()` (the monitor is already in scope —
  the DRR scheduler receives one in plan 008).

### Config — remove the knob
- Delete `Backend.llamacpp_slots` (field + `Default` entry), its `validate()`
  `Some(0)` check, the `config.example.yaml` commented line, and the three
  `tests/config.rs` `llamacpp_slots_*` tests.

### Behavior / edge cases
- **Cold start:** before the first successful `/slots` scrape, `slot_count` is
  `None` ⇒ requests auto-select (`id_slot` omitted). Self-resolves within one
  `metrics_interval`.
- **`/slots` unavailable:** `slot_count` stays `None` ⇒ pinning off (graceful,
  same degradation model as plan 008's KV pressure).
- **Staleness:** `slot_count` is at most one `metrics_interval` old; the real
  slot count is static in practice, so this is a non-issue.
- **vLLM:** the monitor only fetches `/slots` for llama.cpp scrapes
  (`found_llamacpp`), so `slot_count` stays `None` for vLLM ⇒ never pinned.

## Task breakdown

| # | Task | Files | Complexity |
|---|------|-------|-----------|
| 01 | Backend: surface `slot_count` in `BackendSnapshot` (refactor `/slots` fetch) | src/backend/mod.rs | S |
| 02 | Gateway reads live count; remove `llamacpp_slots` config; update tests | src/gateway/mod.rs, src/gateway/proxy.rs, src/main.rs, src/config/mod.rs, src/config/loader.rs, config.example.yaml, tests/config.rs, tests/slot_pinning.rs | M |
| 03 | Docs: update lat.md (Session Slot Pinning now auto-detects; config.md drops the key; backend.md exposes slot_count), README | lat.md/gateway.md, lat.md/config.md, lat.md/backend.md, README.md, config.example.yaml | S |

Each task: delegate `worker` → review `reviewer` → commit. Tasks 01 and 02 are
ordered (02 reads what 01 surfaces).

## Verification (every task)

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
lat check
```

Regression gates:
- Plan 008 KV-pressure `/slots` behavior (kv_usage derivation + warn-once)
  must remain intact (task 01).
- Plan 009 pinning behavior (named→pinned, ephemeral/disabled→not pinned,
  integer id_slot, retry carry) must remain intact — only the *source* of N
  changes from config to snapshot (task 02). All existing tests pass; the
  slot_pinning tests are updated to inject `slot_count` via a watch channel.

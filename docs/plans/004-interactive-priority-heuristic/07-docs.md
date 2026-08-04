# 07 — Docs: PRIORITY.md + WORKLOG rollup

**Phase:** 5 (documentation)
**Depends on:** `04`, `05`, `06`.
**Blocks:** none (closes the plan).

## Objective

Write the operator-facing documentation for the priority heuristic and
the `X-LLM-Priority` header, then update the local vLLM deployment's
`WORKLOG.md` so the next session knows what landed.

## Files

| File | Change |
| --- | --- |
| `docs/plans/001-tinyllb/PRIORITY.md` | NEW: operator-facing doc. |
| `/home/gary/opt/vllm/WORKLOG.md` | EDIT:prepend a rollup entry. |
| `/home/gary/.config/tinyllb/config.yaml` | EDIT: add the `priority_policy:` stanza. |
| `/home/gary/opt/vllm/install-lb.sh` | EDIT: emit `priority_policy` defaults in the generated config. |
| `src/flow/identify.rs` | EDIT: module-level doc comment referencing PRIORITY.md. |
| `src/flow/cadence.rs` | EDIT: module-level doc comment with the classify table. |

## Steps

1. Write `docs/plans/001-tinyllb/PRIORITY.md` covering:

   - One-paragraph overview: cadence-based auto-classification, with
     an explicit header override.
   - **Header**: `X-LLM-Priority: interactive|agent|background|auto`.
     - Pin unpins on `auto`.
     - Persists for the flow across requests.
     - Works alongside `X-LLM-Flow-ID` (priority does not alter
       identity resolution).
   - **Heuristic**:
     - Cold-start (`< min_samples` requests) keeps
       `default_priority` (50).
     - Median inter-request gap `<= background_gap_max` ⇒
       `background` (10).
     - Median gap `>= interactive_gap_min` ⇒ `interactive` (100).
     - In between ⇒ `agent` (50).
     - Hysteresis: an interactive flow is only demoted after a
       sustained run of fast gaps (last 3 all fast), so a burst of
       quick interactive follow-ups doesn't lose priority.
   - **Config** — the `priority_policy` block with the YAML
     table and the defaults shipped in `install-lb.sh`:
     ```yaml
     priority_policy:
       enabled: true
       interactive_gap_min: 30s
       background_gap_max: 2s
       sample_window: 20
       min_samples: 3
     ```
   - **Disabling**: `priority_policy.enabled: false` stops the
     heuristic; explicit `X-LLM-Priority` header still works.
   - **Starvation guarantee**: the existing 300s
     `starvation_timeout` force-admits any flow regardless of
     priority class — `background` never means "starved forever."
   - **Metrics** — three new series:
     - `llm_flow_priority_class{flow_id=...}` gauge
     - `llm_flow_priority_source_total{flow_id=...,source=...}`
       counter (`heuristic` / `header` / `admin` / `auto`)
     - `llm_flow_inter_request_seconds{flow_id=...}` histogram
   - **Tuning guide**:
     - If interactive flows still feel slow under batch load,
       widen `interactive_gap_min` or tighten `background_gap_max`
       to make demotion stricter.
     - If batch flows starve too long under heavy interactive load,
       lower `starvation_timeout` (be careful — too low defeats the
       priority signal).
     - The `min_samples` is the cold-start window; raise to make
       the heuristic more conservative, lower to engage faster.

2. Prepend a `WORKLOG.md` entry in reverse-chronological order
   (newest at the top), matching the existing format. The entry
   should cover:
   - What changed (interactive-vs-batch heuristic + header).
   - Why (the 2026-08-03 19:47 contention burst).
   - New config block to add to `~/.config/tinyllb/config.yaml`.
   - How to disable (`priority_policy.enabled: false`).
   - Link to `docs/plans/001-tinyllb/PRIORITY.md` (or the
     archive summary once the plan completes).

3. Edit `~/.config/tinyllb/config.yaml`: append the
   `priority_policy:` block from step 1 and restart the service:

   ```bash
   systemctl --user restart tinyllb.service
   journalctl --user -u tinyllb --since "1 minute ago" --no-pager
   ```

   Verify the new config is loaded in the `config loaded cfg=...`
   log line on startup.

4. Edit `/home/gary/opt/vllm/install-lb.sh` so the script's
   generated config emits the `priority_policy:` block by default.
   This keeps a future reinstall from losing the policy.

5. Add module-level doc comments to:
   - `src/flow/identify.rs` — note the `X-LLM-Priority` header
     behavior with a link to `PRIORITY.md`.
   - `src/flow/cadence.rs` — describe the cadence table with the
     three boundary cases.

## Verification

- A network-local read of `PRIORITY.md` standalone answers every
  question an operator might have about why a flow got demoted or how
  to override it.
- `WORKLOG.md` top entry references this plan.
- Proxy log on restart shows `priority_policy` populated in the
  config dump (not the defaults, but the configured values).
- A quick sanity request with `X-LLM-Priority: interactive` yields
  `llm_flow_priority_class{...} 100` on the next metrics scrape.

## Notes

- No code changes in this issue except doc comments — the behavior
  shipped is frozen by 06's green test suite.
- If during 01–06 we discover that the hysteresis rule needs
  tweaking or the gap defaults need adjusting for the production
  2-GPU setup, capture that here so the WORKLOG rollup reflects
  reality, not the original guess.

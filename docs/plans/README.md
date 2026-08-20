# Plans

Historical implementation plans. Plans marked superseded/archived/deleted are kept as build records; see the status column for whether a plan reflects current code.

> **Live documentation:** `001-tinyllb/PRIORITY.md` is LIVE operator documentation (not historical) — do not treat it as a build record.

| Plan | Title | Status | Notes |
|---|---|---|---|
| 001-tinyllb | Workspace build-out | Implemented (build record) | FIFO (05) and WFQ (10) phases later deleted 2026-08-19/20 (`c9c4164`, `303bb24`); DRR is the sole surviving scheduler. Facade collapsed to DRR-only (`a3d9eee`). |
| 002 | Context compression | Archived, then deleted | Archived 2026-08-04 as "implemented"; subsystem deleted 2026-08-19 (`d400a93`). See `archive/002-context-compression.md`. |
| 003-session-fingerprint | Session fingerprinting | Implemented | Flow identification via headers/body metadata. |
| 004-interactive-priority-heuristic | Interactive-vs-batch priority heuristic | Implemented, superseded by 006 | Classification logic and config schema changed in plan 006. See `004/PLAN.md` supersession note. |
| 005-premature-stop-retry | Premature-stop retry | Implemented | `src/gateway/retry.rs`, `retry_policy` config, `tinyllb_premature_stop_*` metrics. Note: plan diverged from code (see 005/PLAN.md banner). |
| 006-turn-boundary-priority | Turn-boundary priority state machine | Implemented | Cadence state machine in `src/flow/cadence.rs`; KV-cache-aware selection bias. Plan written while WFQ still existed — scheduler snippets are historical. |

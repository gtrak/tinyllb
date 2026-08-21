# 05 — Docs, lat.md, config example, issue archive

- **Complexity:** S
- **Timebox:** 45 min
- **Depends on:** 01-04 (docs must reflect final behavior)

## Objective

Bring `lat.md/`, `config.example.yaml`, and `README.md` in line with the
new behavior, pass `lat check`, and archive the resolved issue file.

## Files

| File | Change |
|------|--------|
| `lat.md/backend.md` (`# Backend KV-Cache Monitor`) | /slots polling for llama.cpp, derived `kv_usage` + `kv_unified` denominator, inertness when `/slots` unavailable, `snapshot_receiver()`, warn-once behavior. |
| `lat.md/scheduler_policies.md` | NEW top-level section `# KV-Pressure Concurrency Cap` (full format below). |
| `lat.md/scheduler.md` (`# Deficit Round Robin Discipline`) | Cap-aware permit check (`active < effective_cap`) + snapshot wake source; `max_permits` field. |
| `lat.md/config.md` | `backend.kv_unified`, `scheduler.kv_pressure` (+ validation rules). |
| `lat.md/metrics.md` | `llm_backend_kv_pressure`, `scheduler_effective_max_flows` gauges. |
| `lat.md/admission.md` (`# KV-Cache-Aware Admission Gate`) | Constraint note: on llama.cpp `kv_usage` now derives from `/slots` (the gate is no longer inert on that backend when enabled). |
| `config.example.yaml` | `backend.kv_unified: false` (commented rationale); `scheduler.kv_pressure` block with the issue's example ladder, `enabled: false` by default. |
| `README.md` | llama.cpp quickstart: add `--slots` to the server flags; note `kv_unified: true` mirrors `-kvu`; one line on the new `kv_pressure` ladder. |
| `issues/03-dynamic-concurrency-kv-pressure.md` | Move to `issues/archive/`. |

## lat.md section format (required)

Every section needs a leading paragraph (≤250 chars, excluding wiki-link
content) before any child heading. The new `# KV-Pressure Concurrency Cap`
section follows the house format used by
`# KV-Cache-Aware Selection Bias` (lat.md/scheduler_policies.md:258+):

```
# KV-Pressure Concurrency Cap

<leading paragraph: maps KV pressure to an effective max_active_flows
ceiling; soft cap — holds new admits, never preempts; disabled by default.>

## Purpose
## Non-goals            (not admission control; no preemption; not a
                         replacement for the KV gate)
## Interface            (PressureCapHandle::pressure / effective_max /
                         effective; DRR permit condition `active < cap`;
                         wake sources notify + snapshot changed(); closed-
                         channel fallback; gauge)
## Invariants           (disabled ⇒ cap == max_active_flows ⇒ identical
                         behavior; in-flight never aborted; cap read once
                         per admission round; cap never exceeds
                         max_active_flows; starvation force-admit also
                         bounded by the cap)
## Constraints          (staleness = one metrics_interval; /slots requires
                         --slots; kv_unified must mirror -kvu or pressure
                         is mis-scaled; pressure source shared with
                         kv_bias + kv gate)
## Rationale
## Related              (links: [[admission#KV-Cache-Aware Admission Gate]],
                         [[scheduler_policies#KV-Cache-Aware Selection
                         Bias]], [[scheduler#Deficit Round Robin
                         Discipline]], [[backend#Backend KV-Cache Monitor]],
                         [[src/scheduler/pressure_cap.rs#PressureCapHandle]],
                         [[src/config/mod.rs#KvPressure]])
```

Update cross-links in existing sections' `## Related` lists where the new
section belongs (e.g. `# KV-Cache-Aware Selection Bias#Related` and
`# Deficit Round Robin Discipline#Related`).

## Code refs

`pressure_cap.rs` already carries `// @lat: [[scheduler_policies#KV-Pressure
Concurrency Cap]]` (added in task 02). Verify the section id matches exactly
what `lat check` resolves; add `// @lat:` refs to any test that pins a
spec-section behavior if the house convention requires it (check how
`scheduler_policies` test sections are referenced — follow the existing
pattern, do not invent new conventions).

## Steps

1. Update the six `lat.md/` files (new section + targeted edits).
2. `config.example.yaml` — insert `kv_unified` under `backend:` (after
   `metrics_interval`/`stall_timeout`, before `transient_retry`) and the
   `kv_pressure` block under `scheduler:` (after `kv_bias`), with the
   example ladder and comments.
3. `README.md` — llama.cpp quickstart section: add `--slots` to the flag
   list and the two config notes (find the section added in plan 007).
4. `git mv issues/03-dynamic-concurrency-kv-pressure.md issues/archive/`.
5. Run `lat check` — fix every error it reports.

## Verification

```bash
lat check
```

Must print "All checks passed". `cargo test --all` must still pass (no code
changes in this task beyond none — if a `// @lat:` ref forces a code
comment, that is allowed and expected).

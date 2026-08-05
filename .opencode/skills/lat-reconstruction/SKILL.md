---
name: lat-reconstruction
description: Reconstruct codebases into invariant-driven concept graphs with lat.md-compatible specs. Enforces role separation (Extractor, Synthesizer, Auditor) and the "No How" constraint.
compatibility: opencode
---

## Workflows

All workflow files live in `.lat-reverse/workflows/`:

| Workflow | Purpose | When |
|---|---|---|
| `reconstruction.md` | Shared rules: roles, constraints, wiki links, lifecycle, edge types | Loaded by all other workflows |
| `style.md` | Shared formatting: required sections, compression, wiki link syntax | Loaded by synthesize + integrate |
| `split.md` | Decompose a scope into concept candidates | User runs `/lat-rev-split` or asks to decompose a subsystem |
| `add.md` | Identify a single concept from a file or small scope | User runs `/lat-rev-add` or asks to add one concept |
| `auto.md` | Full autonomous pipeline: split → reconstruct all → integrate | User runs `/lat-rev-auto` to map an entire codebase |
| `extract.md` | Extract evidence-backed facts from source (Extractor role) | Phase 1 of `/lat-rev-reconstruct` |
| `synthesize.md` | Produce lat-style spec from extraction (Synthesizer role) | Phase 2 of `/lat-rev-reconstruct` |
| `audit.md` | Compare spec against code, classify findings (Auditor role) | Phase 3 of `/lat-rev-reconstruct` |
| `integrate.md` | Write audited specs into lat.md/, annotate source files | User runs `/lat-rev-integrate` |

## Commands

| Command | What it does |
|---|---|
| `/lat-rev-split [$scope]` | Decompose a scope into candidates — delegates exploration to subagent |
| `/lat-rev-add $1` | Identify a single concept from a file or small scope |
| `/lat-rev-auto [$scope]` | Full autonomous pipeline — split, reconstruct all, integrate (pauses on overlap) |
| `/lat-rev-reconstruct $1` | Full pipeline (extract → synthesize → audit) — each phase in isolated subagent |
| `/lat-rev-integrate [$1]` | Write audited specs to lat.md/ with overlap detection |
| `/lat-rev-next [$1]` | Show status + recommended next action |

## Concept lifecycle

```
candidate → extracted → specified → audited
```

State is in `.lat-reverse/state.json`. CLI is at `.lat-reverse/bin/lat-rev.ts` (run via `bun run .lat-reverse/bin/lat-rev.ts`).

## CLI

```
lat-rev concept add <id> --name <name> --files <f1,f2,...>
lat-rev concept promote <id> --phase <extracted|specified|audited>
lat-rev concept reset <id>
lat-rev concept edge <id> <edge_type> <target_id>
lat-rev status
lat-rev concept show <id>
lat-rev drift [<concept_id>]
lat-rev snapshot <concept_id>
```

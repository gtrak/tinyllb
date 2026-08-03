# Auto Workflow

## Goal

Run the full reconstruction pipeline autonomously: split → reconstruct all candidates → integrate. No review gates (except integrate overlaps). Skips concepts that already exist in `state.json`.

## Scoping

`$scope` is optional — directory, glob, or natural language. Default: entire project.

## Skipping existing concepts

Before proposing new candidates, check `state.json`. Any concept ID that already exists (at any phase) is skipped. Only truly uncovered files produce new candidates.

## Continuation rules

- **Never stop mid-pipeline.** After completing each concept, immediately proceed to the next. DO NOT stop, summarize, or ask for permission between concepts.
- The workflow is complete only when `status` shows no candidates/extracted/specified remaining.
- **Process in groups of 10–20 concepts.** After each group, output a one-line progress count (e.g., "Completed 20/142 candidates") and continue immediately.
- **If interrupted**, re-running this workflow resumes automatically — already-processed concepts are skipped by the pipeline-aware checks.

## Pipeline

### 1. Split (no review gate)

If scope has >20 files or >3 directories, use the hierarchical strategy from `split.md`: one explore subagent per directory, then cross-cutting candidates. If scope is small, a single subagent is fine.

Add all candidates at once using `add-batch`:
```
echo '[...]' | bun run .lat-reverse/bin/lat-rev.ts concept add-batch --file -
```

Add edges via `bun run .lat-reverse/bin/lat-rev.ts concept edge`.

Verify coverage: run `bun run .lat-reverse/bin/lat-rev.ts concept coverage --json`. If any files are uncovered, propose additional candidates and add-batch again.

### 2. Reconstruct all candidates (no review gates)

For each concept with `phase: "candidate"`, run the full reconstruct pipeline. Use the subagent prompt templates from each phase workflow (`extract.md` → `general`, `synthesize.md` → `general`, `audit.md` → `general`). No review gates — auto-approve each phase.

Auto-correct on audit: if audit found `bug` or `spec_error` findings, re-launch synthesis using the **auto-correct prompt template** from `synthesize.md` with the audit findings + original extraction. Write corrected spec. Re-run audit. Repeat until clean or only `undocumented_behavior` findings remain (max 3 cycles). Then promote.

Per-concept sequence:
1. Extract → write extraction.md → promote to extracted
2. Synthesize → write spec.md → promote to specified
3. Audit → write audit.md → auto-correct if needed → promote to audited
4. Snapshot

After processing a group of concepts, promote and snapshot in bulk. Before each `promote-batch`, run `concept list --phase <source_phase> --json` — if empty (concepts already promoted past that phase), skip that promote-batch call and proceed:
```
bun run .lat-reverse/bin/lat-rev.ts concept promote-batch --from <phase> --to <phase>
bun run .lat-reverse/bin/lat-rev.ts snapshot --all
```

### 3. Integrate (pause on overlap conflicts)

Write all audited concepts to `lat.md/` first — do not resolve placeholders until all concepts in the batch are written. Follow wiki link syntax from reconstruction.md: no `.md` extension, no `src/` prefix unless the file is under `src/`, link to files not directories, verify paths exist before writing.

If overlap is detected with existing `lat.md/` content, **pause and ask the user** to resolve. Otherwise, write to `lat.md/`, annotate source files, resolve placeholders (check batch first, then `lat locate`), update index files, and run `lat check`.

After writing all concepts to `lat.md/`, promote in bulk:
```
bun run .lat-reverse/bin/lat-rev.ts concept promote-batch --from audited --to integrated
```

## Rules

- Each phase uses `general` subagents for all three phases (extract, synthesize, audit).
- **Subagents write their own output files directly** and return only `"Written: <path>"`. The orchestrator passes file paths between phases, not file content. This keeps the orchestrator's context window small regardless of file size.
- All state changes (promote, snapshot) go through the CLI.
- On integrate overlap: always pause. Never auto-merge.
- Use batch CLI commands (`add-batch`, `promote-batch`, `snapshot --all`) instead of per-concept calls.
- Auto-correct flow: if audit finds `bug` or `spec_error`, read the audit.md and extraction.md files and pass their *paths* to the re-synthesis subagent (not their content). Re-synthesis reads both files itself and overwrites spec.md. Re-audit reads the new spec.md. Repeat max 3 cycles, then promote.

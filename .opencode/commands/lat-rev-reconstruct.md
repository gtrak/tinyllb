---
description: Walk the full per-concept pipeline (extraction → spec → audit) with isolated subagents and review gates
---

Execute this workflow now. Do not describe it — perform each step.

`$1`: concept_id (required). If already `audited`, restart from extraction.

If the concept's `source_sha` doesn't match git HEAD, run `bun run .lat-reverse/bin/lat-rev.ts drift <concept_id> --json`, present drift to user, let them decide before proceeding.

Do NOT do extraction, synthesis, or audit work yourself. Each phase is a `build` subagent that reads files and returns its output as text. Your job: launch subagents using the prompt templates from each workflow, write their output to disk, present it for review, promote via CLI.

### Phase 1 — Extraction

1. Read `.lat-reverse/workflows/extract.md` — it contains the subagent prompt template.
2. Launch a `build` subagent (type from `extract.md`) using the prompt template from the extract workflow. Fill in the concept's `source_files` and paste `reconstruction.md` content.
3. Write the returned content to `.lat-reverse/concepts/<concept_id>/extraction.md`.
4. **Review gate**: Output extraction as normal text. Use `question` tool: "Approve extraction?" with `Approve` / `I have feedback`. If feedback, re-launch with feedback in prompt.
5. After approval, run `bun run .lat-reverse/bin/lat-rev.ts concept promote <concept_id> --phase extracted`.

### Phase 2 — Synthesis

1. Read `.lat-reverse/workflows/synthesize.md` — it contains the subagent prompt template.
2. Launch a `build` subagent (type from `synthesize.md`) using the prompt template from the synthesize workflow. Fill in the extraction content inline and paste `reconstruction.md` + `style.md` content.
3. Write the returned content to `.lat-reverse/concepts/<concept_id>/spec.md`.
4. **Review gate**: Output spec as normal text. Use `question` tool: "Approve spec?" with `Approve` / `I have feedback`. If feedback, re-launch.
5. After approval, run `bun run .lat-reverse/bin/lat-rev.ts concept promote <concept_id> --phase specified`.

### Phase 3 — Audit

1. Read `.lat-reverse/workflows/audit.md` — it contains the subagent prompt template and review gate options.
2. Launch a `build` subagent (type from `audit.md`) using the prompt template from the audit workflow. Fill in the spec content and source file paths, paste `reconstruction.md` content.
3. Write the returned content to `.lat-reverse/concepts/<concept_id>/audit.md`.
4. **Review gate**: Output audit as normal text, highlighting any findings.
   - If **no issues found**: promote immediately.
   - If **issues found**: use `question` tool: "Audit found issues. How to proceed?" with `Fix spec (re-synthesize)` / `Fix code (I'll edit, then re-audit)` / `Accept findings (promote with caveats)`.
     - **Fix spec**: re-launch synthesis subagent using the auto-correct prompt template from `synthesize.md`, then re-run audit.
     - **Fix code**: wait for user to edit source files, then re-run audit.
     - **Accept findings**: promote. The audit.md records the known gaps.
5. Run `bun run .lat-reverse/bin/lat-rev.ts concept promote <concept_id> --phase audited`.

### Completion

Run `bun run .lat-reverse/bin/lat-rev.ts snapshot <concept_id>` to record the current source SHA. Update edges via `bun run .lat-reverse/bin/lat-rev.ts concept edge` if any emerged during phases.

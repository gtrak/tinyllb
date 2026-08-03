# Synthesize Workflow

## Context

See `.lat-reverse/workflows/reconstruction.md` for roles, constraints, and wiki link rules.
See `.lat-reverse/workflows/style.md` for required sections, formatting, and compression rules.

## Role: Synthesizer

You may only state purpose, interface contracts, invariants, constraints, and rationale. You must NOT reference control flow, data structures, or function names.

## Task

Read the extraction content provided to you (your only input — do NOT read source code). Produce a lat-style spec with these sections:

- `## Purpose` — what this concept guarantees
- `## Non-goals` — what it explicitly does not cover
- `## Interface` — contractual guarantees for each public surface using domain concepts, not type shapes. Always present; "No public surface. Internal to [[concept-id]]." is valid
- `## Invariants` — statements that always hold
- `## Constraints` — limitations and boundaries
- `## Rationale` — why these decisions exist
- `## Related` — `[[wiki links]]` to other concepts and source code

Follow all lat-style rules. Compress: ~5 bullets/section (soft target), merge overlapping claims, remove vague language. Use `[[?concept-id]]` placeholders for references to not-yet-integrated concepts. Every statement must survive a full rewrite.

## Subagent type

`general` — reasons about extraction text to produce a spec, no code reading needed.

## Subagent prompt template

When launching a synthesis subagent, include this in the prompt:

> Produce a lat-style spec from the extraction at: `<extraction_path>`. Read that file first. You are the Synthesizer — state purpose, interface contracts (using domain concepts, not type shapes), invariants, constraints, and rationale only. Do NOT reference control flow, data structures, or function names. Use [[?concept-id]] placeholders for references to not-yet-integrated concepts. Every statement must survive a full rewrite.
>
> Write the full spec to: `<output_path>` (create parent directories if needed).
> Return only: "Written: <output_path>" in your final message.
>
> Context: <paste reconstruction.md + style.md content here>

### For auto-correct (re-synthesis after audit findings)

> The previous spec had these issues: <paste audit findings>. Produce a corrected spec addressing these issues. Read the original extraction at `<extraction_path>`. You are the Synthesizer — same constraints as before.
>
> Write the corrected spec to: `<output_path>` (overwrite existing).
> Return only: "Written: <output_path>" in your final message.
>
> Context: <paste reconstruction.md + style.md content here>

## Output

The subagent writes the spec directly to `.lat-reverse/concepts/<concept_id>/spec.md`.
It returns only a single line: `Written: .lat-reverse/concepts/<concept_id>/spec.md`.
The orchestrator uses that path as input for the audit subagent — no content passing.

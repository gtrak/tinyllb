# Add Workflow

## Goal

Identify a single concept from a specific file or small scope. Lighter than split — no pipeline-awareness, no drift checking. Just: look at this scope, propose one concept.

## Subagent type

`explore` — examines a file or small scope to identify a concept.

## Candidate format

- `concept_id` — unique kebab-case ID
- `name` — human-readable name
- `source_files` — files this concept was extracted from
- `edges` — relationships to existing concepts (if any)

Enforce unique IDs — check `state.json` for collisions.

## Output

Return the candidate as text in your final message. The orchestrator will add it via `bun run .lat-reverse/bin/lat-rev.ts concept add`.

# LAT Style Context

## Section structure

Every section must open with a summary paragraph before any ### child headings. Subagents often skip this and jump straight to bullet points — do not.

Correct:
```
## Invariants
The task state machine guarantees consistent transitions.
### Valid transitions
- ...
```

Wrong (no leading paragraph before ###):
```
## Invariants
### Valid transitions
- ...
```

## Required sections

Every concept spec must contain:

- `## Purpose` — what this concept guarantees
- `## Non-goals` — what it explicitly does not cover
- `## Interface` — contractual guarantees for each public surface using domain concepts, not type shapes. Always present; "No public surface." is valid
- `## Invariants` — statements that always hold
- `## Constraints` — limitations and boundaries
- `## Rationale` — why these decisions exist
- `## Related` — `[[wiki links]]` to other concepts and source code

## Rules

- No implementation leakage
- No vague language ("handles errors", "works correctly", "various things")
- Aggressive redundancy removal
- Source code wiki links (`[[path/relative/to/repo/root#symbol]]` — no `.md` extension, path relative to repo root, must be a file not a directory) only in `Related` sections
- No function/method names as concept identifiers
- Interface section describes domain concepts and contractual shape, not verbatim type definitions
- Every statement must survive a complete rewrite of the implementation

## Compression

- Max ~5 bullets per section (soft target)
- Merge overlapping claims
- Remove vague qualifiers
- Prefer precise statements over broad ones

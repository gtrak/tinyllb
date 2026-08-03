# Split Workflow

## Goal

Decompose a scope into concept candidates. Each candidate represents a distinct unit of responsibility defined by behavior or guarantee, not by file structure.

## Scoping

`$scope` controls how much of the codebase to decompose:

- **No scope** — decompose the entire project
- **Directory** (e.g., `src/auth/`) — decompose only that subsystem
- **Glob** (e.g., `src/**/*.ts`) — decompose matching files
- **Natural language** (e.g., "the authentication layer") — identify relevant files first, then decompose

When scoped to a subsystem, only propose concepts whose `source_files` fall within that scope. Do not propose concepts that span outside the scope — those should be split separately or added individually via `/lat-rev-add`.

## Granularity targets

- Each concept should cover **1–8 source files**. This is a soft target, not a hard limit.
- If a candidate would cover **>10 files**, split it further. Large modules decompose into multiple concepts (e.g., `c_auth_session`, `c_auth_validation`, `c_auth_persistence` rather than `c_auth`).
- A concept covering a single file is fine if that file has distinct responsibilities.
- Every source file in the scope must appear in at least one concept's `source_files`. After proposing candidates, verify full coverage.

## Hierarchical strategy for large scopes

For scopes with >20 source files or >3 directories:

1. **First pass — identify subsystems**: List top-level directories/modules. Each becomes a split scope.
2. **Second pass — decompose each subsystem**: For each directory, launch an explore subagent scoped to that directory only. Produce candidates per the granularity targets above.
3. **Third pass — cross-cutting concepts**: Some responsibilities span directories (e.g., error handling, logging). Propose these last, with `source_files` from across directories.

This ensures each subagent reads a small enough scope to identify fine-grained responsibilities.

## Interface-aware decomposition

Prefer concept boundaries that align with interface surfaces:

- Each distinct API surface (HTTP endpoint group, exported module API, trait impl) is a strong concept boundary signal.
- Extension points (plugin hooks, callback contracts, config schemas) deserve their own concept if multiple consumers depend on the contract.
- Internal code with no public surface is a weaker boundary signal — check whether it represents a genuine contract before making it a standalone concept.

## Pipeline-aware behavior

Before proposing concepts, check existing state:

1. Read `.lat-reverse/state.json` to find existing concepts.
2. Run `bun run .lat-reverse/bin/lat-rev.ts drift --json` to identify stale concepts.
3. For each source file:
   - Already covered by audited concepts whose sources haven't drifted → **skip**
   - Covered by stale concepts → **flag** (suggest `/lat-rev-reconstruct <id>` instead of re-splitting)
   - No coverage → **propose new concepts**

## Coverage verification

After proposing candidates, run `bun run .lat-reverse/bin/lat-rev.ts concept coverage --json` to check for uncovered source files. If any files in the scope are uncovered, propose additional candidates.

## Subagent type

`explore` — surveys the codebase to identify responsibilities and relationships.

## Candidate format

Each candidate has:

- `concept_id` — unique kebab-case ID (e.g., `c_playfield`, `c_auth_validation`)
- `name` — human-readable name
- `source_files` — files this concept was extracted from
- `edges` — inferred relationships to other concepts (existing or new)

Enforce unique IDs. Disambiguate when two scopes produce candidates with the same name.

## Adding candidates

For a single candidate, use:
```
bun run .lat-reverse/bin/lat-rev.ts concept add <id> --name "<name>" --files <f1,f2,...>
```

For multiple candidates, use `add-batch` with a JSON array on stdin:
```
echo '[{"id":"c_x","name":"X","source_files":["a.ts","b.ts"]}]' | bun run .lat-reverse/bin/lat-rev.ts concept add-batch --file -
```

Or from a file:
```
bun run .lat-reverse/bin/lat-rev.ts concept add-batch --file candidates.json
```

Add edges via:
```
bun run .lat-reverse/bin/lat-rev.ts concept edge <id> <edge_type> <target_id>
```

## Output

Return the candidate list as text in your final message. The orchestrator will add them via the CLI and run `concept edge` for any relationships.

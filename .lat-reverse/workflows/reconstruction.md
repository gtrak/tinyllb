# LAT Reconstruction Context

## Roles

| Role | Allowed | Forbidden |
|---|---|---|
| Extractor | Observable behavior, interface surfaces, code evidence, failure modes | Intent, rationale, "why" |
| Synthesizer | Purpose, interface contracts, invariants, constraints, rationale | Control flow, data structures, function names |
| Auditor | Contradictions, mismatches, violations, interface gaps, implementation leakage | Rewriting, fixing, suggesting implementation |

## Invariant validity constraint

All statements must remain true if the implementation is completely rewritten.

## "No How" constraint

Reject outputs that include:
- Control flow descriptions
- Data structure details
- Function/method names as concept identifiers
- Implementation-specific terminology

## Interface-first principle

Concepts capture contracts — what consumers rely on. See "Interface definition" below.

## Interface definition

An interface is a **contract boundary** — the set of guarantees that external
consumers depend on. It is not what the code does internally, but what it
promises to the outside.

### What counts as an interface

Identify these surfaces in source files:

- **HTTP/RPC endpoints** — method + path, request semantics, response
  semantics, status codes, auth requirements
- **Exported functions and methods** — purpose, preconditions,
  postconditions, error contracts
- **Exported types and interfaces** — domain concept each type represents,
  key guarantees (e.g., "identity is immutable after creation"), not field lists
- **Trait implementations** — which trait, behavioral guarantees beyond
  the trait signature
- **Event/message schemas** — published/consumed event semantics,
  payload meaning
- **Configuration contracts** — required config keys, their domain meaning,
  valid ranges, defaults, failure behavior on invalid config
- **Plugin/extension points** — hook semantics, callback contracts,
  registration API guarantees

### What goes in the Interface section

For each surface, state the **contractual guarantee** using domain concepts,
not type shapes:

- What inputs are accepted and what they must satisfy (preconditions)
- What outputs are produced and what they guarantee (postconditions)
- What errors or failure modes are possible and what they mean to the caller
- What invariants hold across the surface

Do NOT include:

- Verbatim type definitions or field lists
- Internal algorithm details or control flow
- Private/helper function signatures
- Implementation-specific type details (e.g., "uses a HashMap internally")
- Step-by-step process descriptions

The docs are an annotated map — given the docs and the source code, a reader
should be able to reconstruct a detailed plan for rewrite. Describe the domain
concepts and contractual shape, not the structural details.

### Interface surface vs. internal behavior

Ask: "If this code were completely rewritten, what would callers still need
to be true?"

- Everything callers depend on → Interface section
- Everything the code happens to do internally → Invariants or Constraints
- If nothing external depends on it → "No public surface. Internal to
  [[concept-id]]."

## Compression rules

- Max ~5 bullets per section (soft target, not a hard limit)
- Merge overlapping claims
- Remove redundancy aggressively
- No vague language ("handles errors", "works correctly")

## Wiki link rules

### Internal wiki links

- Format: `[[path/to/file#Section#SubSection]]`
- **Never** include `.md` extension in wiki links
- ✅ `[[foundation/core#Task State Machine]]`
- ❌ `[[foundation/core.md#Task State Machine]]`

### Source code links

- Path relative to **repo root** — no `src/` prefix unless the file actually lives under `src/`
- Must point to a **file**, not a directory
- **Verify the file path exists** in the source repo before writing
- ✅ `[[crates/rot-core/src/types.rs#TaskState]]`
- ❌ `[[src/crates/rot-core/src/types.rs#TaskState]]`
- ❌ `[[crates/rot-api]]` (directory, not a file)

### Index file entries

- `[[name]]` — no `.md` extension
- ✅ `[[core]]`
- ❌ `[[core.md]]`

### Placeholder refs

- `[[?concept-id]]` — resolved during integrate
- Reference concepts not yet in `lat.md/`. Unresolved placeholders remain as `[[?...]]` and surface as `lat check md` failures.

### Scope restriction

- Source code wiki links allowed **only** in `Related` sections. Banned from Purpose, Interface, Invariants, Constraints, and Rationale.

## `@lat:` source annotations

`// @lat: [[lat.md/path#Concept]]` annotations in source files at relevant symbol locations. Language-appropriate comment prefix (`//` for TS/JS/Go, `#` for Python/Rust). Same link syntax as wiki links — no `.md` extension. Verified via `lat check code-refs`.

## Concept lifecycle

```
candidate → extracted → specified → audited → integrated
```

Progression requires the prerequisite artifact to exist and be approved by the user. Re-running reconstruction on an already-audited concept restarts from scratch. Feedback is handled inline during review gates — no separate revise step.

## Edge types

- `depends_on` — this concept requires another to function
- `refines` — this concept is a specialization/extension of another
- `constrains` — this concept places constraints on another

Cross-concept invariants can be captured as edge annotations on `constrains` relationships, or they emerge during integrate when concepts are co-located.

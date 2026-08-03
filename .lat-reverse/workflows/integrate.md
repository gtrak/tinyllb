# Integrate Workflow

## Context

See `.lat-reverse/workflows/reconstruction.md` for wiki link rules, `@lat:` annotation format, and edge types.
See `.lat-reverse/workflows/style.md` for section structure and formatting.

## Goal

Write audited concepts into the project's `lat.md/` directory. Create or edit sections based on overlap detection. Annotate source files with `@lat:` references.

## Input

Read `.lat-reverse/state.json` to find concepts with `phase: "audited"`. Optional `$1` concept_id to integrate a single concept.

## Overlap detection (three layers)

For each concept being integrated:

1. **`lat locate`** — Run `lat locate <concept_name> --dir <source_repo>` for name-based match.
2. **`lat search`** — If embedding DB is configured, run `lat search "<concept purpose>" --dir <source_repo>` for semantic match.
3. **Explore subagent** — For any matches from layers 1-2, launch an explore subagent to read matched sections from `lat.md/`, compare claims against the new spec, and report: which claims match, which diverge, which are missing from each side.

**No auto-merge** — always present both versions and let the user decide how to resolve.

## Write to lat.md/

For each concept (after overlap resolution):

1. Read the concept's `spec.md` from `.lat-reverse/concepts/<concept_id>/spec.md`.
2. Create or edit the appropriate file in `lat.md/` (e.g., `lat.md/playfield.md`).
3. Use the concept name as the top-level heading.
4. Preserve existing content — never delete existing wiki links from sections being edited.
5. Preserve the leading paragraph of existing sections (append new content below).
6. Source code wiki links (`[[path/relative/to/repo/root#symbol]]` — no `.md` extension, no `src/` prefix unless file is under `src/`, must be a file not a directory) only in `Related` sections. See reconstruction.md for full link syntax.
7. Interface sections describe domain concepts and contractual shape, not verbatim type definitions. Preserve this distinction when editing existing lat.md/ content.

## Source annotation

For each concept, write `@lat:` annotations into the source files listed in `source_files`:

1. Identify the primary symbol(s) related to this concept.
2. Insert `// @lat: [[lat.md/path#Concept#Section]]` (or `# @lat:` for Python/Rust) at the symbol's line.
3. Verify with `lat check code-refs` post-integrate.

## Placeholder resolution

After all concepts in the batch are written to `lat.md/`:

1. Build a map of all concept IDs integrated in this batch → their `lat.md/` file paths.
2. Scan all integrated files for `[[?...]]` placeholders.
3. Resolve each placeholder:
   - First, check if the target was integrated in this batch (use the map from step 1).
   - If not, check if it already exists in `lat.md/` via `lat locate`.
   - Resolve `[[?concept-id]]` → `[[concept-id]]` or the appropriate `[[file#Section]]` link where possible.
4. Remaining `[[?...]]` placeholders stay unresolved — they surface as `lat check md` failures.

## Index file maintenance

After creating or editing any `.md` in `lat.md/`:

1. Update the parent directory's index file (e.g., `lat.md/api/api.md`).
2. Add `- [[name]] — description` entry for new files.
3. Remove entries for deleted files.
4. If index file doesn't exist, create it with proper format.
5. Run `lat check index` to verify.

## Post-integrate

Always run `lat check --dir <source_repo>`. Report any errors (unresolved placeholders, broken links, invalid code refs) for manual resolution.

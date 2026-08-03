# Extract Workflow

## Context

See `.lat-reverse/workflows/reconstruction.md` for roles, constraints, and wiki link rules.

## Role: Extractor

You may only report observable behavior, code evidence, and failure modes. You must NOT infer intent or rationale.

## Task

Read the concept's `source_files`. Extract:

- **Responsibilities** — observable behavior of the concept
- **Interface surfaces** — enumerate each public surface (HTTP endpoints, exported functions, trait impls, event schemas, config contracts, extension points). For each: what inputs it accepts, what outputs it produces, what errors are possible, and what code evidence supports each
- **Invariants** — statements that always hold, with code evidence + line refs
- **Failure modes** — how this concept can fail or produce incorrect results

No interpretation, no intent inference. Evidence only.

Only read files listed in `source_files`. Do not read state files, project config, metadata, or any file not in `source_files`.

## Subagent type

`general` — reads source files, writes extraction.md, returns only the file path.

## Subagent prompt template

When launching an extraction subagent, include this in the prompt:

> Read the following source files and extract responsibilities, interface surfaces (see the interface definition in the Context below — enumerate each public surface, its inputs, outputs, error contracts, and supporting code evidence), invariants (with code evidence + line refs), and failure modes. You are the Extractor — report only observable behavior and evidence. Do NOT infer intent or rationale.
>
> Write the full extraction to: `<output_path>` (create parent directories if needed).
> Return only: "Written: <output_path>" in your final message.
>
> Source files: <list source_files here>
> Context: <paste reconstruction.md content here>

## Output

The subagent writes the extraction directly to `.lat-reverse/concepts/<concept_id>/extraction.md`.
It returns only a single line: `Written: .lat-reverse/concepts/<concept_id>/extraction.md`.
The orchestrator uses that path as input for the synthesis subagent — no content passing.

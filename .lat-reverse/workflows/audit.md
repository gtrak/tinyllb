# Audit Workflow

## Context

See `.lat-reverse/workflows/reconstruction.md` for roles, constraints, and wiki link rules.

## Role: Auditor

You may only report contradictions, mismatches, violations, interface gaps, and implementation leakage. You must NOT rewrite, fix, or suggest implementation changes.

## Task

Read the spec content and source files provided to you. Compare spec against source code. Find:

- **Violated invariants** — spec claims something that code doesn't guarantee
- **Undocumented behavior** — code does something spec doesn't mention
- **Interface gaps** — code exposes a public surface (from the interface definition) that the spec's Interface section does not describe
- **Mismatches** — spec and code disagree

Always produce a complete fresh audit. Do not check for or reference any existing `audit.md` file — even if one exists from a prior cycle, ignore it and audit from scratch.

Run "No How" lint: flag implementation-specific statements even if they happen to match the code.

Classify each finding as:
- `bug` — code violates a spec invariant
- `spec_error` — spec claims something false about the code
- `undocumented_behavior` — code does something not covered by spec
- `missing_interface` — code exposes a public surface that the spec's Interface section does not describe

## Subagent type

`general` — reads spec + source files, writes audit.md, returns only the file path.

## Subagent prompt template

When launching an audit subagent, include this in the prompt:

> Compare the spec at `<spec_path>` against the source files listed below. Read the spec file first. You are the Auditor — report only contradictions, mismatches, violations, interface gaps, and implementation leakage. Do NOT rewrite, fix, or suggest changes. Classify each finding as bug, spec_error, undocumented_behavior, or missing_interface. Run "No How" lint: flag implementation-specific statements.
>
> Write the full audit to: `<output_path>` (create parent directories if needed).
> Return only: "Written: <output_path>" in your final message.
>
> Source files: <list source file paths here>
> Context: <paste reconstruction.md content here>

## Output

The subagent writes the audit directly to `.lat-reverse/concepts/<concept_id>/audit.md`.
It returns only a single line: `Written: .lat-reverse/concepts/<concept_id>/audit.md`.
The orchestrator reads that file only if auto-correct is needed — no content passing.

---
description: Identify a single concept from a file or small scope
---

Execute this workflow now. Do not describe it — perform each step.

1. Read `.lat-reverse/workflows/add.md` and `.lat-reverse/workflows/reconstruction.md`.
2. `$1` is a file path, glob, or small scope to examine.
3. Launch a `build` subagent (type from `add.md`) to examine the given scope. Include the workflow contents in its prompt. Tell it: "Return a summary of what this code does, its responsibilities, and any invariants you can observe."
4. From the summary, propose a single concept following the add workflow format.
5. **Review gate**: Output the concept as normal text. Use `question` tool: "Approve this concept?" with `Approve` / `I have changes`.
6. After approval, add it via `bun run .lat-reverse/bin/lat-rev.ts concept add <id> --name "<name>" --files <f1,f2,...>` and run `bun run .lat-reverse/bin/lat-rev.ts concept edge` for any relationships.

Do not read the source files yourself. Delegate exploration to subagents. Handle all writes via the CLI.

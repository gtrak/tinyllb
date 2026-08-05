---
description: Decompose a scope into concept candidates with source files and inferred graph edges
---

Execute this workflow now. Do not describe it — perform each step.

1. Read `.lat-reverse/workflows/split.md` and `.lat-reverse/workflows/reconstruction.md`.
2. `$scope` is optional — directory, glob, or natural language. Default: entire project. For a single file, use `/lat-rev-add` instead.
3. **If scope has >20 files or >3 directories**: first list top-level directories, then launch one `build` explore subagent per directory scoped to that directory only. Each subagent returns candidates for its directory. After all subagents complete, propose any cross-cutting concepts. **If scope is small (<20 files)**: a single explore subagent is fine.
4. From the summaries, propose concept candidates following the split workflow format. **Target 1–8 source files per concept.** If a candidate would cover >10 files, split it further.
5. **Review gate**: Output candidates as normal text. Use `question` tool: "Approve these candidates?" with `Approve all` / `I have changes`. Do NOT put candidates inside the question tool.
6. After approval, add all candidates at once using `add-batch`:
   ```
   echo '[{"id":"c_x","name":"X","source_files":["a.ts"]}]' | bun run .lat-reverse/bin/lat-rev.ts concept add-batch --file -
   ```
   Or for a single candidate: `bun run .lat-reverse/bin/lat-rev.ts concept add <id> --name "<name>" --files <f1,f2,...>`.
7. Add edges via `bun run .lat-reverse/bin/lat-rev.ts concept edge <id> <edge_type> <target_id>`.
8. Verify coverage: run `bun run .lat-reverse/bin/lat-rev.ts concept coverage --json`. If any source files are uncovered, propose additional candidates.

Do not explore the codebase yourself. Delegate all file reading to explore subagents. Handle all writes via the CLI.

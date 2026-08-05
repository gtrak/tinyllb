---
description: Show workflow status and recommended next action
---

Execute `bun run .lat-reverse/bin/lat-rev.ts status --json` now. If the user provided an argument (a concept ID), execute `bun run .lat-reverse/bin/lat-rev.ts concept show <argument> --json` instead.

Take the JSON output and format it for the user:

**No argument** — phase summary with all concepts:
- Phase summary with count per phase
- List each concept with its ID, phase, and staleness
- Recommended next command with a specific concept ID

Example:
```
Phase: candidate(3) extracted(1) specified(2) audited(1)
  c_playfield → specified (Grid-Based Playfield)
  c_row_clearing → candidate STALE (Row Clearing)
  c_scoring → candidate (Scoring)
Recommended: /lat-rev-reconstruct c_row_clearing (source changed since last extraction)
```

**With concept_id** — single concept detail:
- Phase, staleness, changed files, edges, recommended next step

Example:
```
c_playfield → specified → Next: /lat-rev-reconstruct c_playfield will run audit phase
```

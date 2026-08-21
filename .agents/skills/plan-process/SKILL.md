---
name: plan-process
description: >-
  Planning pattern for feature development: active plan structure,
  issue file format, and archival process when a plan is implemented.
---

# Plan Process

The planning workflow for feature development.  Each plan lives in a
numbered folder under `docs/plans/` with a `PLAN.md` overview and broken-down
issue files, then gets archived once implemented.

## Pattern

### Plan Numbering

Scan `docs/plans/` and `docs/plans/archive/` for all existing numbers, take the highest `NNN`, and add 1:

```bash
# Active plan folders (docs/plans/NNN-*/)
ls docs/plans/ | grep -E '^[0-9]+' | sed 's/-.*//' | sort -n | tail -1

# Archived plans (docs/plans/archive/NNN-*.md)
ls docs/plans/archive/ | grep -E '^[0-9]+' | sed 's/-.*//' | sort -n | tail -1
```

Use the greater of the two +1. Example: if highest is `021`, next plan is `022`.

### Active Plan

```
docs/plans/NNN-short-name/
  PLAN.md            # Overview: why, what, approach, scope, task order
  01-first-step.md   # Actionable issue with objective, files, steps
  02-next-step.md    # ...
  ...
```

**PLAN.md** — High-level design document covering:
- **Why** — problem statement and motivation
- **What** — technical approach and scope
- **Success criteria** — how we know it's done
- **Task order** — dependency graph for the subtasks

**Numbered issue files** — Self-contained, actionable work items:
- **Objective** — what this step accomplishes
- **Files** — table of affected files and changes
- **Steps** — ordered implementation steps
- **Verification** — how to confirm correctness

### Completion

When a plan is fully implemented, consolidate `PLAN.md` + all issue files into a
single short summary (~20-30 lines), write it to `archive/NNN-short-name.md`,
then remove the original folder:

```bash
# Archive summary includes:
# - 1-2 paragraphs explaining what and why
# - Brief scope bullets (major areas, not every file)
# - Phase/task list (headings only, no detail)
```

The archive is a flat list of `.md` files — one per completed plan.  Full
implementation details remain in git history.

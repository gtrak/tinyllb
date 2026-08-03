# Issue 03 — Docs: Session Fingerprinting

## Objective

Document the new flow-identity sources so operators and future
maintainers know which headers are honored and in what order.

## Files

| File | Change |
|------|--------|
| `docs/plans/001-llm-qdisc-proxy/TRACING.md` | Update `flow_id` field description to mention harness session headers |
| `src/flow/identify.rs` | Module doc comment: full precedence list (done in issue 01; verify wording) |
| `README.md` (if present) | Note supported session headers in the flow-ID section, if one exists |

## Steps

1. **`TRACING.md`** — the `flow_id` field (around line 51) currently
   says "header, metadata, or ephemeral". Extend to: "resolved flow
   identifier — `X-LLM-Flow-ID`, harness session headers
   (`x-claude-code-session-id`, `x-session-id`, `x-session-affinity`,
   `x-client-request-id`, `session_id`), `metadata.flow_id`, or
   ephemeral UUID".

2. **`identify.rs` module doc comment** — confirm it lists all 8
   precedence levels from PLAN.md, and note header matching is
   case-insensitive and empty values fall through.

3. **Changelog** — if the repo keeps one, add a one-line entry:
   "flow identification now honors agentic harness session headers
   (Claude Code, opencode, pi) and the standard `X-Session-Id`".

## Verification

```bash
rg -n "x-session|claude-code|session-affinity" docs/ README.md 2>/dev/null
cargo build --all-targets   # docs only, but confirm no accidental edits
```
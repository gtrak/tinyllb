# 03 — Docs: lat.md section + cross-refs, README, config example, archive issue 04

- **Complexity:** S
- **Timebox:** 40 min
- **Depends on:** 01, 02

## Objective

Document the feature in `lat.md`, keep `config.example.yaml`/`README.md` in
sync, pass `lat check`, and archive the resolved issue.

## Files

| File | Change |
|------|--------|
| `lat.md/gateway.md` | NEW section `# Session Slot Pinning` (full format below). |
| `lat.md/flow.md` | Add cross-link from `# Flow Identifier Contract` (a flow id now also selects a backend slot). |
| `lat.md/config.md` | Document `backend.llamacpp_slots`. |
| `config.example.yaml` | (Verify task 01's commented `llamacpp_slots` is present and correct; adjust if needed.) |
| `README.md` | llama.cpp quickstart: one line on `llamacpp_slots` for session pinning. |
| `issues/04-id-slot-session-pinning.md` | Move to `issues/archive/` via `git mv`. |

## lat.md section format (required)

Every section needs a leading paragraph (≤250 chars, excluding wiki-link
content) before any child heading. Follow the house format used by the other
`gateway.md` sections (e.g. `# Transient Backend-Error Re-forward`,
`lat.md/gateway.md:551`). Add the new section `# Session Slot Pinning` in a
sensible position (near the request-handling / retry sections):

```
# Session Slot Pinning

<leading paragraph: pins a named session to a stable llama.cpp slot via the
id_slot request field so its prompt KV cache reuses across turns; disabled by
default; ephemeral requests auto-select.>

## Purpose
## Non-goals            (no per-slot KV attribution/observation; no free-list
                         allocation; vLLM unaffected)
## Interface            (backend.llamacpp_slots: Option<u32>;
                         slot_id_for_flow(flow, n) = fnv1a % n;
                         id_slot injected as a JSON integer into the forwarded
                         body for named inference requests only)
## Invariants           (disabled/None ⇒ no id_slot ⇒ byte-identical;
                         deterministic across restarts; id_slot ∈ [0, n);
                         ephemeral + non-inference never pinned; id_slot baked
                         into forwarded_body so retries carry it; vLLM never
                         receives it)
## Constraints          (n should mirror --parallel; n higher than real count
                         wraps in llama.cpp (id_slot % slots.size()), lower
                         under-uses slots; deterministic hash required because
                         the randomized HashMap hasher re-shuffles on restart)
## Rationale            (why pin: prompt KV-cache reuse → lower TTFT on
                         follow-up turns; why hash over free-list: stateless,
                         no lifecycle coupling; why config over /slots
                         auto-detect: predictable, self-gating, no cold-start)
## Related              ([[gateway#Reverse Proxy Request Handling]],
                         [[flow#Flow Identifier Contract]],
                         [[config#...]] (the config section that holds backend
                         keys), [[src/flow/mod.rs#slot_id_for_flow]],
                         [[src/gateway/proxy.rs#inject_id_slot]])
```

Add the new section to the `## Related` lists of the sections it genuinely
belongs with (e.g. `# Reverse Proxy Request Handling#Related`, and from
`lat.md/flow.md` `# Flow Identifier Contract#Related`).

## Code refs

Add a `// @lat: [[gateway#Session Slot Pinning]]` comment on
`slot_id_for_flow` (src/flow/mod.rs) and/or on `inject_id_slot`
(src/gateway/proxy.rs) so the section is code-anchored. Verify the section id
matches exactly what `lat check` resolves. Follow the existing `// @lat:`
convention used elsewhere (check a neighboring file). If the house convention
requires the ref on a specific symbol, put it there; one ref is enough to
anchor the section — do not over-annotate.

## Steps

1. Add the new `# Session Slot Pinning` section to `lat.md/gateway.md`.
2. Add cross-links in `lat.md/flow.md` and `lat.md/config.md` (and any `Related`
   lists that should include it).
3. Add the `// @lat:` code ref(s).
4. `config.example.yaml` — confirm the commented `llamacpp_slots` is present and
   accurate (task 01 added it).
5. `README.md` — llama.cpp quickstart: one line that `backend.llamacpp_slots: N`
   (mirror `--parallel`) pins named sessions to a stable slot for KV-cache
   reuse; ephemeral requests keep auto-selection.
6. `git mv issues/04-id-slot-session-pinning.md issues/archive/`.
7. Run `lat check` — fix every error. Must end "All checks passed".

## Verification

```bash
lat check                 # must be "All checks passed"
cargo test --all          # must still pass (only a possible // @lat: comment added)
cargo clippy --all-targets -- -D warnings
```

# 01 — Rust Workspace + CI Gate

**Phase:** 0 (Foundation)
**Blocks:** all subsequent issues.

## Objective

Stand up the Rust workspace skeleton, the single command that CI (and the
agent) runs for lint + typecheck + tests, and a minimal `main.rs` that boots an
axum server returning `200 OK` on `/healthz`.  This is the foundation every
later issue builds on; nothing else can be developed until the workspace
compiles and the CI gate is green.

## Files

| File | Change |
| --- | --- |
| `Cargo.toml` | New workspace root; crate `llm-qdisc-proxy`, edition 2021. |
| `Cargo.lock` | Generated on first build; commit. |
| `src/main.rs` | New: minimal axum server, `/healthz` -> `200 OK`, listens on `0.0.0.0:8080` (port overridable via env). |
| `.gitignore` | New: `target/`, `/target`. |
| `AGENTS.md` | New: document the lint/typecheck/test command for the agent (see Verification). |
| `.github/workflows/ci.yml` | New: build + lint + test on push/PR. |

## Steps

1. `cargo init --name llm-qdisc-proxy`.
2. Add deps to `Cargo.toml`: `axum`, `tokio` (full), `serde`, `serde_json`,
   `tower`, `reqwest` (for later proxying; pull now to fail fast on version
   conflicts), `prometheus` (later, but reserve the dep), `tracing`,
   `tracing-subscriber`, `clap` (derive), `config` (or `figment`) for cfg.
   Pin a recent MSRV.
3. Write `src/main.rs`: axum router with `GET /healthz` -> `"ok"`; bind to
   `0.0.0.0:8080` or `$PORT`.  Wire `tracing_subscriber::fmt().init()`.
4. `cargo build && cargo clippy --all-targets -- -D warnings && cargo test --all`.
5. Add `.github/workflows/ci.yml`: `rust-toolchain` action, run
   `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`.
6. Create `AGENTS.md` with a "Commands" section listing the exact lint,
   typecheck, and test commands so the agent runs them after edits.

## Verification

* `cargo fmt --check` clean.
* `cargo clippy --all-targets -- -D warnings` clean.
* `cargo test --all` passes (the trivial health test).
* `cargo run` then `curl localhost:8080/healthz` returns `ok`.
* CI workflow is green on a no-op PR.
* `AGENTS.md` lists the three commands the agent should run after edits.

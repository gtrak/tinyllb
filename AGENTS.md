# Agent Commands

Run these three commands after every edit to verify correctness.

## Lint

```bash
cargo clippy --all-targets -- -D warnings
```

## Typecheck

```bash
cargo build --all-targets
```

## Test

```bash
cargo test --all
```

These commands run from `/home/gary/dev/vllm-frontend` (repo root).

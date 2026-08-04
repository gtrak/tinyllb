# 19 — Dockerfile + Deployment Docs (Local Single-GPU, Multi-GPU Local)

**Phase:** Cross-cutting — land with Phase 1 ship; revisit Phase 3.
**Depends on:** `01`.
**Blocks:** (none — complements issues; required for `07` reproducibility).

## Objective

Make the proxy one-command-runnable and containerized, matching the
deployment shapes in PRD §10:

* Local single GPU: `agent -> proxy -> vllm -> GPU`.
* Multi-GPU local: `proxy -> vllm TP=2 -> 5060Ti pair` (docs only; the proxy
  itself doesn't care about TP — it just forwards to one URL).

PRD §3 explicitly excludes UI dashboards and distributed clusters, so the
deployment story stays local-first.

## Files

| File | Change |
| --- | --- |
| `Dockerfile` | New: multi-stage, `cargo build` -> minimal runtime image. |
| `docker-compose.yaml` | New: proxy + vLLM example (vLLM image env vars). |
| `config.example.yaml` | Edit: ensure it matches the docker default bind. |
| `README.md` | New: quickstart, single-GPU + multi-GPU deploys, env var reference. |
| `scripts/run_local.sh` | New: one-shot local single-GPU launcher. |

## Steps

1. `Dockerfile`: builder stage uses `rust:1.x-slim`; runtime stage uses
   `debian:bookworm-slim` + the compiled binary + `config.example.yaml`.
   Final image is non-root, EXPOSE 8080, ENTRYPOINT the binary with
   `--config /etc/tinyllb/config.yaml`.
2. `docker-compose.yaml` example:
   * `vllm` service using `vllm/vllm-openai:latest` with appropriate `--model`
     and GPU reservation,
   * `proxy` service depends_on vllm, `CONFIG_PATH=/etc/tinyllb/config.yaml`,
     healthcheck hitting `/healthz`.
3. `README.md` (wait until `03` lands so quickstart shows a real curl):
   * Quickstart: `scripts/run_local.sh` then `curl ...`,
   * Single-GPU deploy via compose,
   * Multi-GPU: example env vars for `vllm` (`--tensor-parallel-size=2`) and
     the note that the proxy sees one backend URL,
   * Env var reference (mirror `02`'s `TINYLLB__*` override list),
   * Link to `docs/plans/.../PHASE*-RESULTS.md` for measured throughput.
4. `scripts/run_local.sh`: cargo run + default config + assumes vLLM at
   `http://localhost:8000`; print the curl example.
5. Revisit in Phase 3 to add a `scripts/phase3_bench.sh` companion that
   uses the Dockerfile to keep the live bench environment identical to
   production.

## Verification

* `docker build -t tinyllb .` succeeds; image size is reasonable
  (<50MB runtime layer) and runs as non-root.
* `docker compose up` brings up proxy + vllm; `curl localhost:8080/v1/models`
   works after vLLM is ready.
* `scripts/run_local.sh` works on a dev machine with cargo.
* README quickstart is copy-paste-runnable against a fresh checkout with a
  vLLM already running.

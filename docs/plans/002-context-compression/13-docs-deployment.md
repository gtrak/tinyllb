# Issue 13 — Documentation + Deployment Config

## Objective

Update all documentation and deployment scripts to reflect the context
compression feature. This includes config examples, the install script,
AGENTS.md, WORKLOG.md, and any operational runbooks.

## Files

| File | Change |
|------|--------|
| `config.example.yaml` (vllm-frontend) | Add `context_policy` section with full comments |
| `/home/gary/opt/vllm/install-lb.sh` | Generate `context_policy` config, set tokenizer_path |
| `/home/gary/opt/vllm/AGENTS.md` | Add context compression description, admin API docs |
| `/home/gary/opt/vllm/WORKLOG.md` | Add implementation entry |
| `README.md` (vllm-frontend, if exists) | Add context compression section |

## Prerequisites

- All implementation issues 01–12

## Steps

1. **`config.example.yaml`** (in vllm-frontend repo):
   Add the full `context_policy` section with inline documentation:
   ```yaml
   # Context compression: reduces per-flow context size by summarizing
   # older conversation turns. Keeps head (system prompt + early turns)
   # and live (recent turns) verbatim; compresses the middle.
   # See docs/plans/002-context-compression/PLAN.md for full design.
   context_policy:
     enabled: false                # Set to true to activate
     compress_threshold: 100000    # Est. tokens to trigger compression
     head_keep_turns: 3            # Turns kept verbatim at start (prefix cache anchor)
     live_keep_turns: 6            # Turns kept verbatim at end (recent context)
     compress_chunk_turns: 8      # Turns folded into each summary
     summary_max_tokens: 2048     # Max tokens for sidecar summarization request
     store_path: "~/.local/share/llm-qdisc/transcripts.db"
     tokenizer_path: null          # Path to tokenizer.json for accurate counts
     sidecar_request_timeout: 60s
     compression_retries: 3
     prompt_template_path: null    # Optional: custom summarization prompt
   ```

2. **`install-lb.sh`** (in `/home/gary/opt/vllm/`):
   Add `context_policy` section to the generated config:
   ```bash
   cat >> "$CONFIG_DIR/config.yaml" <<'YAML'

   context_policy:
     enabled: ${LB_COMPRESS_ENABLED:-false}
     compress_threshold: ${LB_COMPRESS_THRESHOLD:-100000}
     head_keep_turns: 3
     live_keep_turns: 6
     compress_chunk_turns: 8
     summary_max_tokens: 2048
     store_path: "$HOME/.local/share/llm-qdisc/transcripts.db"
     tokenizer_path: "$MODEL_DIR/$MODEL_NAME/tokenizer.json"
     sidecar_request_timeout: 60s
     compression_retries: 3
   YAML
   ```
   - `MODEL_DIR` and `MODEL_NAME` are already variables in install-lb.sh
     (or can be derived from the model path)
   - Create the store directory: `mkdir -p "$HOME/.local/share/llm-qdisc"`
   - Add env var `LB_COMPRESS_ENABLED` so the user can toggle compression
     without editing the config file

3. **`AGENTS.md`** (in `/home/gary/opt/vllm/`):
   Add to the key files table:
   ```
   | `install-lb.sh` context_policy | Context compression config. `LB_COMPRESS_ENABLED=true` to activate. See `docs/plans/002-context-compression/` in vllm-frontend. |
   ```
   Add a new critical constraint:
   ```
   6. **Context compression is DISABLED by default.** Enable via
      `LB_COMPRESS_ENABLED=true` in the environment before running
      `install-lb.sh install`. The proxy maintains per-flow transcripts in
      SQLite and summarizes older turns via sidecar requests to vLLM (tagged
      as `background`-priority). Compressed segments are immutable and
      prefix-cache-friendly. Admin API: `GET /admin/context/{flow_id}` for
      inspection. Fails open: if the compression subsystem errors, the proxy
      forwards the original request unchanged.
   ```
   Add admin API commands to the Commands section:
   ```bash
   # Inspect context state for a flow:
   curl http://localhost:1234/admin/context/{flow_id} | jq

   # List all flows with token counts:
   curl http://localhost:1234/admin/context | jq

   # Force-trigger compression for a flow:
   curl -X POST http://localhost:1234/admin/context/{flow_id}/compress

   # Clear a flow's transcript:
   curl -X DELETE http://localhost:1234/admin/context/{flow_id}
   ```

4. **`WORKLOG.md`** (in `/home/gary/opt/vllm/`):
   Add entry at the top:
   ```markdown
   ## 2026-08-XX — Context compression implemented

   **Feature:** The proxy (`llm-qdisc-proxy`) now supports per-flow context
   compression. When a flow's estimated token count exceeds `compress_threshold`
   (default 100K), older turns are summarized via a background sidecar request
   to vLLM (tagged `background`-priority). The summary replaces the original
   turns in forwarded requests.

   **Design:** Segment-based transcript model `[Head + Compressed₁..ₙ + Live]`.
   Head and Compressed segments are immutable (prefix-cache-friendly). Live
   segment grows per turn; oldest chunk folded into a new Compressed segment
   when threshold exceeded. Transcripts stored in SQLite via `sqlx`.

   **Config:** `context_policy` section in `~/.config/llm-qdisc/config.yaml`.
   Enable with `LB_COMPRESS_ENABLED=true`. Tokenizer loaded from model dir.

   **Admin API:** `GET /admin/context/{flow_id}` for inspection,
   `POST /admin/context/{flow_id}/compress` to force-trigger.

   **Status:** Implemented, disabled by default. Enable for testing.
   ```

5. **README.md** (in vllm-frontend, if it exists):
   Add a "Context Compression" section with:
   - Brief description of the feature
   - How to enable (config + `enabled: true`)
   - How it works (segment model, prefix cache friendliness)
   - Admin API endpoints
   - Prometheus metrics
   - Link to `docs/plans/002-context-compression/PLAN.md`

6. **Config validation docs** — add to AGENTS.md or a separate doc:
   Document the valid ranges for each config field:
   - `compress_threshold`: 1000–500000 (must be < model's max_model_len)
   - `head_keep_turns`: 1–20
   - `live_keep_turns`: 1–50
   - `compress_chunk_turns`: 1–50 (must be < live_keep_turns for meaningful compression)
   - `summary_max_tokens`: 256–8192
   - `compression_retries`: 1–10

## Verification

```bash
# Config example is valid YAML:
python3 -c "import yaml; yaml.safe_load(open('config.example.yaml'))"

# install-lb.sh generates valid config:
bash /home/gary/opt/vllm/install-lb.sh install
cat ~/.config/llm-qdisc/config.yaml | grep context_policy

# AGENTS.md mentions context compression:
grep -c "context compression" /home/gary/opt/vllm/AGENTS.md

# WORKLOG.md has the entry:
grep "Context compression" /home/gary/opt/vllm/WORKLOG.md
```

## Notes

- `LB_COMPRESS_ENABLED` is an env var consumed by `install-lb.sh` during
  config generation, not by the proxy at runtime. To toggle at runtime,
  edit `~/.config/llm-qdisc/config.yaml` and `systemctl --user restart
  llm-qdisc-proxy.service`.
- The `tokenizer_path` in the generated config points to the model's
  `tokenizer.json` — this is already on disk at
  `/home/gary/opt/vllm/models/Qwen3.6-27B-PrismaAURA-5.5bit-vllm/tokenizer.json`.
- When enabling compression, ensure the SQLite store directory exists and
  is writable: `mkdir -p ~/.local/share/llm-qdisc`.

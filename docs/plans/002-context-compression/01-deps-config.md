# Issue 01 — Dependencies + Config Schema

## Objective

Add `sqlx` (SQLite) and `tokenizers` (HF) as dependencies, define the
`ContextPolicy` config struct tree, wire it into `Config`, add validation,
and update the example config. Everything downstream depends on this.

## Files

| File | Change |
|------|--------|
| `Cargo.toml` | Add `sqlx`, `tokenizers` deps |
| `src/config/mod.rs` | Add `ContextPolicy` struct, add field to `Config` |
| `src/config/loader.rs` | Add validation for context policy fields |
| `config.example.yaml` | Add `context_policy` section with all fields + comments |
| `src/lib.rs` | Declare `context` module (empty stub for now) |
| `src/context/mod.rs` | Create with `// module stub — populated in issues 02+` |

## Steps

1. **Cargo.toml** — add to `[dependencies]`:
   ```toml
   sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio-rustls"] }
   tokenizers = "0.20"
   ```
   Run `cargo check` to confirm the deps resolve and compile.

2. **`src/context/mod.rs`** — create empty module:
   ```rust
   // Context compression module — populated in issues 02+
   ```
   Add `pub mod context;` to `src/lib.rs`.

3. **`src/config/mod.rs`** — define config structs:
   ```rust
   #[derive(Debug, Clone, serde::Deserialize)]
   pub struct ContextPolicy {
       pub enabled: bool,
       pub compress_threshold: usize,
       pub head_keep_turns: usize,
       pub live_keep_turns: usize,
       pub compress_chunk_turns: usize,
       pub summary_max_tokens: usize,
       pub store_path: String,
       pub tokenizer_path: Option<String>,
       pub sidecar_request_timeout: humantime::Duration,
       pub compression_retries: u32,
       pub prompt_template_path: Option<String>,
   }
   ```
   Add `pub context_policy: ContextPolicy` to `Config`.

4. **Implement `Default` for `ContextPolicy`**:
   ```rust
   impl Default for ContextPolicy {
       fn default() -> Self {
           Self {
               enabled: false,
               compress_threshold: 100_000,
               head_keep_turns: 3,
               live_keep_turns: 6,
               compress_chunk_turns: 8,
               summary_max_tokens: 2048,
               store_path: "~/.local/share/llm-qdisc/transcripts.db".to_string(),
               tokenizer_path: None,
               sidecar_request_timeout: Duration::from_secs(60).into(),
               compression_retries: 3,
               prompt_template_path: None,
           }
       }
   }
   ```

5. **`src/config/loader.rs`** — add validation:
   - If `context_policy.enabled`:
     - `compress_threshold > 0`
     - `head_keep_turns > 0`
     - `live_keep_turns > 0`
     - `compress_chunk_turns > 0`
     - `summary_max_tokens > 0`
     - `store_path` non-empty (expand `~` to home dir)
     - `compression_retries > 0`
   - Expand `~` in `store_path` and `tokenizer_path` to the user's home
     directory using `std::env::var("HOME")`.

6. **`config.example.yaml`** — add section with inline comments:
   ```yaml
   context_policy:
     enabled: false
     compress_threshold: 100000
     head_keep_turns: 3
     live_keep_turns: 6
     compress_chunk_turns: 8
     summary_max_tokens: 2048
     store_path: "~/.local/share/llm-qdisc/transcripts.db"
     tokenizer_path: null  # path to tokenizer.json for accurate token counts
     sidecar_request_timeout: 60s
     compression_retries: 3
     prompt_template_path: null  # optional custom summarization prompt
   ```

7. **Verify**:
   - `cargo check` passes
   - Existing config loads with `context_policy` defaulting to disabled
   - Config validation rejects invalid values (threshold = 0, empty store_path)
   - `~` expansion works in both `store_path` and `tokenizer_path`

## Verification

```bash
cargo check 2>&1 | tail -5
cargo test --lib config 2>&1 | tail -5
```

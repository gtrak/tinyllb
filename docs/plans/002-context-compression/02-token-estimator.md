# Issue 02 — Token Estimator

## Objective

Accurately estimate token counts for arbitrary text and message arrays using
the model's HF tokenizer. This is needed by reconciliation (issue 05) to
decide when to trigger compression, by body rewriting (issue 07) to report
total context size, and by metrics (issue 11).

## Files

| File | Change |
|------|--------|
| `src/context/estimator.rs` | New — `TokenEstimator` struct + methods |
| `src/context/mod.rs` | Add `pub mod estimator;` |
| `src/lib.rs` | Re-export `context::estimator::TokenEstimator` |

## Prerequisites

- Issue 01 (config with `tokenizer_path`)

## Steps

1. **Define `TokenEstimator`** in `src/context/estimator.rs`:
   ```rust
   pub struct TokenEstimator {
       tokenizer: Option<tokenizers::Tokenizer>,
   }
   ```

2. **Constructor** — `TokenEstimator::new(tokenizer_path: Option<&str>) -> Self`:
   - If `tokenizer_path` is `Some(path)` and file exists:
     - Load via `Tokenizer::from_file(path)`
     - On success: `Self { tokenizer: Some(t) }`
     - On failure: log warning, fall back to heuristic: `Self { tokenizer: None }`
   - If `None`:
     - `Self { tokenizer: None }` (heuristic mode)

3. **`estimate_text(&self, text: &str) -> usize`**:
   - If tokenizer available:
     ```rust
     self.tokenizer.encode(text, true)
         .map(|enc| enc.get_ids().len())
         .unwrap_or_else(|_| fallback(text))
     ```
   - Fallback heuristic: `(text.len().max(1) * 10 + 3) / 32` (≈ chars / 3.2,
     tuned for English+code; the BPE ratio for Qwen is ~3.2 chars/token)

4. **`estimate_messages(&self, messages: &[serde_json::Value]) -> usize`**:
   - Sum `estimate_text()` over all `message["content"]` string values
   - Add overhead per message: +4 tokens (role tag + structural tokens)
   - Handle `content` that is an array (multimodal): sum text parts only,
     skip image/video parts

5. **`estimate_turns(&self, messages: &[serde_json::Value]) -> usize`** — sum
   over all message contents + per-message overhead. Same as
   `estimate_messages` but named for clarity at call sites.

6. **Thread safety**: `tokenizers::Tokenizer` is `Send + Sync`. The
   `TokenEstimator` can be freely cloned or wrapped in `Arc` and shared
   across the `AppState`. Use `Arc<TokenEstimator>` in `AppState`.

7. **Unit tests** in `src/context/estimator.rs`:
   - `test_heuristic_basic` — "hello world" returns a positive count
   - `test_tokenizer_loads` — if tokenizer path points to the model dir's
     `tokenizer.json`, confirm it loads and returns accurate counts
   - `test_messages_array` — a 3-message array returns the sum of parts
   - `test_empty_string` — returns 0 (or 1 for BOS)
   - `test_multimodal_content` — array content with text + image URL:
     image parts skipped, text parts counted

## Verification

```bash
cargo test --lib estimator 2>&1 | tail -10
# Manual check against model tokenizer:
cargo test --lib test_tokenizer_loads -- --ignored 2>&1 | tail -5
```

If a real `tokenizer.json` is available at the configured path, the tokenizer
count should match vLLM's own token count for the same input text (within
±2 tokens for structural overhead).

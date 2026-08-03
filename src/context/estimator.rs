//! Token-count estimation for context compression.
//!
//! `TokenEstimator` wraps an optional HuggingFace `tokenizers::Tokenizer`
//! loaded from disk. When a tokenizer is available it produces accurate
//! BPE token counts; otherwise it falls back to a character-ratio heuristic
//! tuned for English + code (Qwen's BPE ratio is ~3.2 chars/token).
//!
//! The estimator is `Send + Sync` (since `tokenizers::Tokenizer` is) and is
//! intended to be shared via `Arc` from `AppState`.

use std::path::Path;

use tokenizers::Tokenizer;

/// Per-message overhead added on top of the content token count.
///
/// Models the role tag plus structural tokens (chat template framing) that
/// the backend would inject for each turn.
const PER_MESSAGE_OVERHEAD: usize = 4;

/// Estimates token counts for arbitrary text and OpenAI-style message arrays.
///
/// Construct with [`TokenEstimator::new`], passing `Some(path)` pointing at a
/// serialized `tokenizer.json` (e.g. a Qwen model's HF tokenizer). If the path
/// is `None`, missing, unreadable, or fails to parse, the estimator falls back
/// to a byte-ratio heuristic so callers never have to handle a load error.
pub struct TokenEstimator {
    tokenizer: Option<Tokenizer>,
}

impl TokenEstimator {
    /// Build an estimator from an optional `tokenizer.json` path.
    ///
    /// - If `tokenizer_path` is `None`, the file is missing, or loading fails,
    ///   the estimator runs in heuristic mode (`tokenizer: None`).
    /// - On success, uses the real tokenizer for exact BPE counts.
    /// - Load failures are logged via `tracing::warn!` but never panic.
    pub fn new(tokenizer_path: Option<&str>) -> Self {
        let path = match tokenizer_path {
            Some(p) => p,
            None => return Self::heuristic(),
        };
        if !Path::new(path).exists() {
            return Self::heuristic();
        }
        match Tokenizer::from_file(path) {
            Ok(tokenizer) => Self {
                tokenizer: Some(tokenizer),
            },
            Err(e) => {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "failed to load tokenizer, falling back to heuristic estimation",
                );
                Self::heuristic()
            }
        }
    }

    fn heuristic() -> Self {
        Self { tokenizer: None }
    }

    /// Estimate the token count of a single text string.
    ///
    /// Uses the real tokenizer when present (encoding with special tokens);
    /// otherwise falls back to `(len.max(1) * 10 + 3) / 32` (chars / ~3.2,
    /// matching Qwen's BPE ratio). Empty input always yields 0.
    pub fn estimate_text(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        match &self.tokenizer {
            Some(tokenizer) => tokenizer
                .encode(text, true)
                .map(|enc| enc.get_ids().len())
                .unwrap_or_else(|_| fallback_heuristic(text)),
            None => fallback_heuristic(text),
        }
    }

    /// Estimate the token count of an OpenAI-style message array.
    ///
    /// Sums `estimate_text()` over each message's `content` and adds
    /// [`PER_MESSAGE_OVERHEAD`] per message (role tag + structural tokens).
    ///
    /// `content` may be:
    /// - a plain string → counted directly,
    /// - an array (multimodal) → only `{"type":"text","text":"..."}` objects
    ///   count; image/video/etc. parts are skipped,
    /// - missing or null → contributes 0 text tokens (but the message still
    ///   incurs [`PER_MESSAGE_OVERHEAD`] if the message object exists).
    ///
    /// Never panics: malformed shapes simply contribute 0.
    pub fn estimate_messages(&self, messages: &[serde_json::Value]) -> usize {
        self.sum_messages(messages)
    }

    /// Alias of [`estimate_messages`](Self::estimate_messages).
    ///
    /// Provided for clarity at call sites that reason about whole conversation
    /// turns. Identical in behavior.
    pub fn estimate_turns(&self, messages: &[serde_json::Value]) -> usize {
        self.sum_messages(messages)
    }

    /// Shared implementation for [`estimate_messages`](Self::estimate_messages)
    /// and [`estimate_turns`](Self::estimate_turns).
    fn sum_messages(&self, messages: &[serde_json::Value]) -> usize {
        let mut total = 0usize;
        for message in messages {
            // Each message object always incurs the structural overhead,
            // even if content is missing/null/empty.
            total += PER_MESSAGE_OVERHEAD;
            total += self.estimate_content(message.get("content"));
        }
        total
    }

    /// Estimate text tokens from one message's `content` field.
    ///
    /// `content` may be `None` (caller passes `None` when the key is absent),
    /// `null`, a string, or a multimodal array. Anything else returns 0.
    fn estimate_content(&self, content: Option<&serde_json::Value>) -> usize {
        let content = match content {
            Some(c) if !c.is_null() => c,
            _ => return 0,
        };
        match content {
            serde_json::Value::String(s) => self.estimate_text(s),
            serde_json::Value::Array(parts) => {
                let mut sum = 0usize;
                for part in parts {
                    if let Some(obj) = part.as_object() {
                        if obj.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                                sum += self.estimate_text(text);
                            }
                        }
                    }
                }
                sum
            }
            // Numbers, booleans, bare objects, etc. carry no text tokens.
            _ => 0,
        }
    }
}

/// Byte-ratio fallback when no tokenizer is available.
///
/// `(len.max(1) * 10 + 3) / 32` ≈ `len / 3.2`, tuned for English + code where
/// Qwen BPE averages ~3.2 chars per token. The `+3` term rounds the integer
/// division so small inputs aren't systematically undercounted.
fn fallback_heuristic(text: &str) -> usize {
    (text.len().max(1) * 10 + 3) / 32
}

// Static guarantee that the estimator is shareable across threads via `Arc`
// from `AppState`. `tokenizers::Tokenizer` is `Send + Sync`; this assert
// surfaces a clear compile error if that ever changes upstream.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TokenEstimator>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heuristic_basic() {
        let est = TokenEstimator::new(None);
        // "hello world" is 11 bytes → (11*10 + 3) / 32 = 113/32 = 3.
        let count = est.estimate_text("hello world");
        assert!(count > 0, "heuristic should return a positive count");
        assert_eq!(count, 3);

        // A non-existent path should also fall back to heuristic mode.
        let est_path = TokenEstimator::new(Some("/does/not/exist/tokenizer.json"));
        assert!(est_path.estimate_text("hello world") > 0);
    }

    #[test]
    fn test_empty_string() {
        let est = TokenEstimator::new(None);
        assert_eq!(est.estimate_text(""), 0);
    }

    #[test]
    fn test_messages_array() {
        let est = TokenEstimator::new(None);
        let messages = serde_json::json!([
            { "role": "system", "content": "you are helpful" },
            { "role": "user", "content": "hello there" },
            { "role": "assistant", "content": "hi! how can I help?" },
        ]);
        let array = messages.as_array().unwrap();

        let expected: usize = ["you are helpful", "hello there", "hi! how can I help?"]
            .iter()
            .map(|s| est.estimate_text(s))
            .sum::<usize>()
            + PER_MESSAGE_OVERHEAD * 3;

        assert_eq!(est.estimate_messages(array), expected);
    }

    #[test]
    fn test_multimodal_content() {
        let est = TokenEstimator::new(None);
        let messages = serde_json::json!([
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "describe this image" },
                    { "type": "image_url", "image_url": { "url": "http://example.com/img.png" } }
                ]
            }
        ]);
        let array = messages.as_array().unwrap();

        // Image part must be skipped; only the text part + per-message overhead.
        let expected = est.estimate_text("describe this image") + PER_MESSAGE_OVERHEAD;
        assert_eq!(est.estimate_messages(array), expected);
    }

    #[test]
    fn test_missing_content() {
        let est = TokenEstimator::new(None);
        // First message has no `content` field → 0 text tokens, still +4 overhead.
        // Second message is null content → 0 text tokens, still +4 overhead.
        let messages = serde_json::json!([
            { "role": "system" },
            { "role": "user", "content": null },
        ]);
        let array = messages.as_array().unwrap();

        let count = est.estimate_messages(array);
        assert_eq!(count, PER_MESSAGE_OVERHEAD * 2);

        // Mixed: missing content + present content.
        let messages2 = serde_json::json!([
            { "role": "system" },
            { "role": "user", "content": "hi" }
        ]);
        let array2 = messages2.as_array().unwrap();
        let expected2 = PER_MESSAGE_OVERHEAD + est.estimate_text("hi") + PER_MESSAGE_OVERHEAD;
        assert_eq!(est.estimate_messages(array2), expected2);
    }

    #[test]
    fn test_estimate_turns_matches_messages() {
        let est = TokenEstimator::new(None);
        let messages = serde_json::json!([
            { "role": "system", "content": "setup" },
            { "role": "user", "content": "question one" },
            { "role": "assistant", "content": "answer one" },
            { "role": "user", "content": "follow up" }
        ]);
        let array = messages.as_array().unwrap();
        assert_eq!(est.estimate_turns(array), est.estimate_messages(array));
    }

    /// Verifies that a real `tokenizer.json` loads and produces token counts.
    ///
    /// This test is `#[ignore]` because it requires a real tokenizer file,
    /// which is not checked into the repository. To run it, set the
    /// `TOKENIZER_PATH` environment variable to a `tokenizer.json` on disk
    /// (e.g. a Qwen model's HF tokenizer), then:
    ///
    /// ```sh
    /// TOKENIZER_PATH=/path/to/tokenizer.json cargo test test_tokenizer_loads -- --ignored --lib
    /// ```
    ///
    /// If `TOKENIZER_PATH` is unset or empty, the test prints a notice and
    /// returns without asserting, so it never fails in CI without a tokenizer.
    #[test]
    #[ignore = "requires TOKENIZER_PATH env var pointing at a real tokenizer.json"]
    fn test_tokenizer_loads() {
        let path = match std::env::var("TOKENIZER_PATH") {
            Ok(p) if !p.is_empty() => p,
            _ => {
                eprintln!(
                    "skipping: set TOKENIZER_PATH to a real tokenizer.json to run this test"
                );
                return;
            }
        };
        let est = TokenEstimator::new(Some(&path));
        let count = est.estimate_text("hello world");
        assert!(
            count > 0,
            "tokenizer loaded from {path} should produce a positive count"
        );
        // Round-trip sanity: a longer string should yield more tokens.
        let longer = est.estimate_text("hello world, this is a much longer sentence");
        assert!(longer > count, "longer input should yield more tokens");
    }
}

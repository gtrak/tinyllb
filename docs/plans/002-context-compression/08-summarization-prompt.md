# Issue 08 — Summarization Prompt

## Objective

Design a default summarization prompt that produces compact, information-rich
summaries of conversation turns. The prompt preserves code references,
decisions, factual context, entity names, and file paths — everything an
agent needs to continue working after compression.

The prompt is configurable via an optional `prompt_template_path` config
field, allowing custom prompts without code changes.

## Files

| File | Change |
|------|--------|
| `src/context/prompt.rs` | New — `build_summarization_prompt()` + default template |
| `src/context/mod.rs` | Add `pub mod prompt;` |

## Prerequisites

- Issue 03 (segment model — messages to compress)

## Steps

1. **Default system prompt** (embedded as a Rust const string):
   ```text
   You are a conversation summarizer. Your task is to compress a segment
   of conversation history into a concise summary that preserves all
   information needed to continue the conversation.

   PRESERVE:
   - Code snippets, file paths, and technical identifiers (verbatim if short,
     described if long)
   - Decisions made and their rationale
   - Factual context: entity names, project names, version numbers, URLs
   - The state of any tasks in progress (what's done, what's pending)
   - Tool calls and their results (summarize the outcome, not the full output)
   - Errors encountered and how they were resolved
   - User preferences and constraints stated in these turns

   COMPRESS:
   - Redundant repetition
   - Verbose explanations (condense to key points)
   - Full code blocks over 20 lines (describe what they do, keep key snippets)
   - Pleasantries and meta-commentary

   FORMAT:
   - Bullet points, grouped by topic if the conversation spans multiple topics
   - Start with "Summary of turns {start}-{end}:"
   - Keep under {max_tokens} tokens

   Do NOT add information not present in the original turns. Do NOT
   speculate or infer beyond what was explicitly stated.
   ```

2. **`build_summarization_prompt()` function**:
   ```rust
   pub fn build_summarization_prompt(
       messages_to_compress: &[serde_json::Value],
       turn_start: usize,
       turn_end: usize,
       max_tokens: usize,
       custom_template: Option<&str>,
   ) -> Vec<serde_json::Value>
   ```

   Returns a `messages` array suitable for the sidecar `POST /v1/chat/completions`:
   ```json
   [
     {
       "role": "system",
       "content": "<template with {start}, {end}, {max_tokens} substituted>"
     },
     {
       "role": "user",
       "content": "<serialized messages_to_compress as a formatted block>"
     }
   ]
   ```

3. **Message serialization for the user content**:
   Format the messages-to-compress as a readable transcript:
   ```
   --- Conversation turns {start} to {end} ---

   [user]: <content>

   [assistant]: <content>

   [tool]: <result summary>

   [assistant]: <content>
   ...
   ```
   - Stringify `content` (handle string and array content types)
   - For tool calls: include the function name and truncated arguments
   - For tool results: include the first 500 chars of the result
   - Truncate individual messages longer than 2000 chars (with "[...truncated]"
     marker)

4. **Template substitution**:
   - Replace `{start}`, `{end}`, `{max_tokens}` in the system prompt template
   - If `custom_template` is provided (from `config.prompt_template_path`):
     - Load file contents at startup (not per-request — cache it)
     - Substitute the same placeholders
     - If file can't be loaded: log error, fall back to default template

5. **Config integration**:
   - The `prompt_template_path` config field (from issue 01) is loaded once
     at `ContextState` initialization
   - Store as `Option<String>` in `ContextState` (the raw template text)
   - `build_summarization_prompt` receives it as `custom_template: Option<&str>`

6. **`SummarizationPromptBuilder`** (optional, for reusability):
   ```rust
   pub struct PromptBuilder {
       template: String,
       max_tokens: usize,
   }

   impl PromptBuilder {
       pub fn new(config: &ContextPolicy) -> Self { ... }
       pub fn build(&self, messages: &[Value], turn_start: usize, turn_end: usize) -> Vec<Value> { ... }
   }
   ```
   Store in `ContextState` as `Arc<PromptBuilder>`.

7. **Unit tests**:
   - `test_default_prompt_has_placeholders` — template contains `{start}`,
     `{end}`, `{max_tokens}`
   - `test_substitution` — placeholders replaced with actual values
   - `test_message_serialization` — messages correctly formatted as
     `[role]: content` blocks
   - `test_truncation` — long messages truncated with marker
   - `test_tool_call_serialization` — tool calls include function name +
     truncated args
   - `test_custom_template` — custom template loaded from file,
     placeholders substituted

## Verification

```bash
cargo test --lib prompt 2>&1 | tail -10
```

## Prompt design rationale

The prompt is tuned for agentic use cases (the primary driver):
- Code references and file paths are critical for coding agents — losing
  them means the agent can't navigate the codebase after compression
- Task state tracking ensures the agent knows what it's done and what's
  left to do
- Tool call summarization prevents bloating the summary with full tool
  outputs (which can be thousands of tokens)
- The format instruction ("bullet points, grouped by topic") produces
  compact, scannable summaries
- The anti-hallucination instruction prevents the summarizer from adding
  incorrect context

# 02 — AppState field + `id_slot` injection in `proxy_handler`

- **Complexity:** M
- **Timebox:** 60 min
- **Depends on:** 01

## Objective

Wire the config into the request path: for a **named** (non-ephemeral)
**inference** request, when `llamacpp_slots` is set, inject `id_slot` (a JSON
integer) into the outgoing backend body, composed with the existing
`include_usage` injection so retries carry it.

## Files

| File | Change |
|------|--------|
| `src/gateway/mod.rs` | `AppState.llamacpp_slots: Option<u32>` + `test_default` sets `None`. |
| `src/gateway/proxy.rs` | `inject_id_slot(...)`; hoist `is_inference`; compute slot; compose into `forwarded_body`; drop `Content-Length` iff body changed. |
| `src/main.rs` | Pass `cfg.backend.llamacpp_slots` into `AppState`. |
| `tests/` (stub-backend) | Integration tests asserting the forwarded body. |

## Context (verified facts — do not re-derive)

- **Injection site** `src/gateway/proxy.rs:485-497` (current shape):
  ```rust
  let mut headers = headers;
  let injected = inject_include_usage(&body_bytes);
  let forwarded_body: Bytes = match &injected {
      Some(b) => b.clone(),
      None => body_bytes.clone(),
  };
  let mut builder = state.client.request(method.clone(), backend_url.clone());
  if let Some(injected) = injected {
      headers.remove(axum::http::header::CONTENT_LENGTH);
      builder = builder.body(injected);
  } else {
      builder = builder.body(body_bytes);
  }
  ```
- `flow_id: FlowId` is in scope from `:461` (`let flow_id = resolved.flow_id;`).
  `flow_id.is_ephemeral()` exists (`src/flow/mod.rs:57`). `&flow_id.to_string()`
  is the id string.
- `is_inference_request(method: &Method, path: &str) -> bool` at
  `src/gateway/proxy.rs:193` (pure). It is currently first called at `:504`
  (the non-inference early-return). Hoist a `let is_inference = ...` before the
  body build and reuse it at `:504` (replace the duplicated call).
- `inject_include_usage(body: &Bytes) -> Option<Bytes>` at `:108-132` is the
  field-injection precedent (parse → mutate object → `to_vec`).
- `forwarded_body` is the canonical retried body: transient retries and
  premature-stop re-forwards re-send it (`proxy.rs:647,706`; `stream.rs:421,615`;
  the premature-stop path re-derives via `bump_temperature`). Baking `id_slot`
  into `forwarded_body` ⇒ retries carry it with no extra work.
- `AppState` is `#[derive(Clone)]` (`src/gateway/mod.rs:19-40`);
  `test_default` (`:48-68`) fills defaults and is used by tests (override via
  struct-update syntax). `main.rs` constructs the real `AppState`.
- `slot_id_for_flow(flow: &str, slot_count: u32) -> u32` is now available from
  task 01 (`crate::flow::slot_id_for_flow`).

## Implementation spec

1. **`src/gateway/mod.rs`**:
   - Add field to `AppState`:
     ```rust
     /// llama.cpp slot count for `id_slot` session pinning (mirrors
     /// `--parallel`). `None` disables pinning. See plan 009.
     pub llamacpp_slots: Option<u32>,
     ```
   - In `test_default`, set `llamacpp_slots: None`.

2. **`src/gateway/proxy.rs`** — add a field-injection helper next to
   `inject_include_usage`:
   ```rust
   /// Inject `id_slot` (integer) into the outgoing body so llama-server pins
   /// the request to a specific slot (session KV-cache reuse). Returns `Some`
   /// with the new body, `None` if the body isn't a JSON object.
   fn inject_id_slot(body: &Bytes, slot: u32) -> Option<Bytes> {
       let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
       value.as_object_mut()?.insert(
           "id_slot".to_string(),
           serde_json::Value::from(slot),
       );
       Some(serde_json::to_vec(&value).ok()?.into())
   }
   ```

3. **Compute the slot** (right after `flow_id` is bound, or just before the body
   build):
   ```rust
   let is_inference = is_inference_request(&method, &original_path);
   let id_slot: Option<u32> = match (is_inference, flow_id.is_ephemeral(), state.llamacpp_slots) {
       (true, false, Some(n)) => Some(slot_id_for_flow(&flow_id.to_string(), n)),
       _ => None,
   };
   ```
   (Import `slot_id_for_flow` — `use crate::flow::slot_id_for_flow;` or
   fully-qualified.)

4. **Compose the body** — replace the `:485-497` block with a single pass that
   applies `include_usage` then `id_slot`, and drops `Content-Length` iff the
   bytes changed:
   ```rust
   let mut headers = headers;
   let mut forwarded_body: Bytes = body_bytes.clone();
   if let Some(b) = inject_include_usage(&forwarded_body) {
       forwarded_body = b;
   }
   if let Some(slot) = id_slot {
       if let Some(b) = inject_id_slot(&forwarded_body, slot) {
           forwarded_body = b;
       }
   }
   let mut builder = state.client.request(method.clone(), backend_url.clone());
   if forwarded_body != body_bytes {
       headers.remove(axum::http::header::CONTENT_LENGTH);
   }
   builder = builder.body(forwarded_body);
   ```
   **Behavior note:** when nothing is injected, `forwarded_body == body_bytes`
   and `Content-Length` is preserved (identical to today). When either
   injection re-serializes, the bytes differ and `Content-Length` is dropped so
   reqwest recomputes it — the existing `include_usage` path already relied on
   dropping it on change, so this is equivalent, not a regression.

5. **Hoist reuse** — at the existing non-inference check (`:504`), replace
   `if !is_inference_request(&method, &original_path)` with `if !is_inference`.

6. **`src/main.rs`** — where the real `AppState` is built, add
   `llamacpp_slots: cfg.backend.llamacpp_slots,`.

## Tests (stub-backend integration)

Follow the existing stub-backend pattern in `tests/gateway.rs` (build an `AppState`
via `AppState::test_default(...)` + struct-update to set `llamacpp_slots`, mount
`create_router()`, point `backend_url` at a local stub axum server that RECORDS
the request body it receives). The stub must capture the raw request JSON so the
test can assert on `id_slot`. If the existing stub doesn't record the body,
extend the test stub (test scaffolding only).

Tests (put in a new `tests/slot_pinning.rs` unless a more natural existing file
fits — check first):
1. `named_session_injects_id_slot` — `llamacpp_slots: Some(4)`, request with
   `x-session-id: ses_a` → forwarded body contains `id_slot` as an integer
   `== slot_id_for_flow("ses_a", 4)`.
2. `same_session_same_slot` — two requests, same `x-session-id` → same `id_slot`
   value in both forwarded bodies.
3. `ephemeral_omits_id_slot` — `llamacpp_slots: Some(4)`, request with NO session
   header (→ ephemeral) → forwarded body has NO `id_slot` key.
4. `disabled_omits_id_slot` — `llamacpp_slots: None`, request with
   `x-session-id: ses_a` → forwarded body has NO `id_slot` key.
5. `id_slot_is_integer` — assert the JSON value type of `id_slot` is a number
   (serde_json `is_i64()`/`is_u64()`), not a string.
6. `models_route_never_pinned` — `GET /v1/models` → no `id_slot` (and confirm it
   is not even attempted).
7. (optional, only if the transient-retry test scaffolding is already present and
   cheap to reuse) `id_slot_survives_retry` — stub returns one transient error
   then success; the retried request still carries the same `id_slot`. If the
   scaffolding is not readily reusable, SKIP and note it (it's structurally
   guaranteed by baking into `forwarded_body`).

Also add an in-file `#[cfg(test)]` unit test for `inject_id_slot` in
`src/gateway/proxy.rs` (add to the existing test module if present): object body
gets `id_slot` inserted; non-object/invalid body → `None`.

## Verification

```bash
cargo clippy --all-targets -- -D warnings
cargo build --all-targets
cargo test --all
lat check
```

**Regression gate (critical):** with `llamacpp_slots: None` (the default, and the
value every pre-existing test uses via `test_default`), NO `id_slot` is injected
and the forwarded body is byte-identical to before ⇒ **all existing tests must
pass unchanged.** If any existing test's forwarded body changes, that is a defect.

`lat check` must stay "All checks passed" (no `// @lat:` added this task; docs
land in task 03).

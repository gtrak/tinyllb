# Audit — c_config_loading

## Metadata

- **Concept**: c_config_loading
- **Auditor**: Final-cycle auditor (LAT reconstruction pipeline)
- **Spec version**: Twice-corrected spec at `.lat-reverse/concepts/c_config_loading/spec.md`
- **Source files audited**: `src/config/loader.rs`, `src/config/mod.rs`
- **Status**: Findings below

---

## "No How" Lint

**Pass.** The spec contains no function names as concept identifiers, no control flow descriptions, no data structure details, and no implementation-specific terminology in Purpose, Interface, Invariants, Constraints, or Rationale sections. Source code wiki links appear exclusively in the Related section, as required.

---

## Findings

### 1. `Server.tps_window_secs` — Missing from spec

**Classification**: `missing_interface`

**Summary**: The `Server` struct in `mod.rs` (line 324) declares a field `tps_window_secs: u64` with a default value of `10` seconds. This field is not mentioned anywhere in the spec — not in the Default configuration contract, not in the Interface section, and not in the Non-goals.

**Evidence**:
- `mod.rs` line 324: `pub tps_window_secs: u64` with `#[serde(default = "Server::default_tps_window_secs")]`
- `mod.rs` line 333: `fn default_tps_window_secs() -> u64 { 10 }`
- No corresponding `set_default("server.tps_window_secs", ...)` in `loader.rs` (defaults via struct `Default` impl, not explicit loader default)
- Spec "Default configuration contract" lists only `bind = 0.0.0.0:8080` under server binding; `tps_window_secs` is absent.

**Impact**: The spec omits a public configuration field that is configurable via YAML or environment variable. A consumer relying solely on the spec would not know this field exists or its default value.

---

### 2. `loader.rs` does not set an explicit default for `server.tps_window_secs`

**Classification**: `undocumented_behavior`

**Summary**: Unlike all other configuration keys (which are explicitly set via `set_default()` calls in the `load()` function), `server.tps_window_secs` has no corresponding `set_default()` call. Its default comes solely from the `Server::default()` impl in `mod.rs`. All other struct defaults in `mod.rs` are redundant with the loader defaults — except this one.

**Evidence**:
- `loader.rs` lines 66–84: 20 `set_default()` calls covering every key except `server.tps_window_secs` and `request_timeout` and `completion_bias` fields.
- `loader.rs` line 81: `set_default("server.bind", "0.0.0.0:8080")` — present, but no `set_default` for `tps_window_secs`.
- The config crate's `try_deserialize()` will fall back to struct defaults for any missing key when `#[serde(default)]` is present, which makes this functionally equivalent — but the asymmetry is noteworthy.

**Impact**: Functional equivalence holds, but the spec's claim "Every configuration key has a built-in default" is technically true (the default exists via the struct) yet the loading mechanism differs from the pattern established for all other keys. This is not a bug — it is an implementation detail the spec does not need to cover. However, the missing field (Finding 1) remains underspecified.

---

### 3. `request_timeout` default mechanism underspecified

**Classification**: `spec_error`

**Summary**: The spec states at the Default configuration contract (line 60): "Request timeout (`None` — absent timeout defers to the HTTP client's built-in timeout, 300s in reqwest)." This conflation is problematic. The configuration value is `None` (no timeout set by this system). The `300s` behaviour is entirely outside the scope of this concept — it is an HTTP client concern, not a configuration loading concern. The spec claims knowledge of `reqwest`'s internal default timeout, which is neither a configuration invariant nor a loading contract.

**Evidence**:
- `mod.rs` line 29: `pub request_timeout: Option<Duration>` with `#[serde(default, with = "loader::humantime_serde_option")]`
- `mod.rs` line 28-27: Comment mentions "Defaults to the reqwest client timeout (300s)" — this is a doc comment on the model, not a loader behavior.
- The spec's claim about `300s` in reqwest is external to the configuration system and cannot be validated from config loading code.

**Impact**: Minor. The spec statement is informative but technically out of scope. If the HTTP client library changes or a different client is used, the `300s` claim becomes stale without any change to the configuration system. The correct spec statement is: "Request timeout defaults to `None`, meaning no application-level timeout is enforced." The HTTP client's own default is not a configuration contract.

---

### 4. `retry_after_base` — spec says "not validated for positivity; zero values pass validation" — Confirmed

**Classification**: `spec_error` (informational — spec is accurate)

**Summary**: The spec states at Invariant line 86: "The non-optional duration field `retry_after_base` is not validated for positivity; zero values pass validation." Inspecting `loader.rs`, there is indeed no validation check for `retry_after_base`. The claim is accurate — this entry is listed for completeness in the audit trail.

**Impact**: None. Recording for audit completeness.

---

### 5. `CompletionBias` fields absent from loader defaults

**Classification**: `undocumented_behavior`

**Summary**: The spec lists `completion_bias` defaults (enabled = true, target_active_flows = 0, predictive_admit = false) in the Default configuration contract. However, `loader.rs` does not call `set_default()` for any of these three fields — they rely entirely on the `CompletionBias::default()` impl. The spec describes the defaults as properties of the loading system (built-in defaults), which is accurate in outcome but slightly imprecise in mechanism since these particular defaults are structural, not loader-provided.

**Evidence**:
- `loader.rs`: No `set_default("scheduler.completion_bias.enabled", ...)` or equivalents.
- `mod.rs` lines 143–168: `CompletionBias::default()` provides all three values.

**Impact**: None. Functional equivalence holds. The spec correctly states the default values; the mechanism of how they are applied (struct default vs. loader default) is an implementation detail the spec need not cover. Noted for completeness.

---

### 6. Backpressure mode enum serialization — `fail_fast` vs. `fail-fast`

**Classification**: `undocumented_behavior`

**Summary**: The spec refers to backpressure modes as `"blocking"`, with the implementation using `#[serde(rename_all = "lowercase")]` on the `BackpressureMode` enum. This produces `"blocking"`, `"fail_fast"`, and `"hybrid"`. The spec's Constraints section (line 110) references modes as "hybrid backpressure mode", "fail-fast mode", and "blocking mode" — using hyphen-separated terms (`fail-fast`) rather than underscore-separated ones (`fail_fast`). However, these references appear in prose (validation rules), not as configuration value literals.

**Evidence**:
- `mod.rs` line 290: `#[serde(rename_all = "lowercase")]` on `BackpressureMode`
- `mod.rs` lines 291-295: `Blocking`, `FailFast`, `Hybrid` → serialized as `"blocking"`, `"fail_fast"`, `"hybrid"`
- Spec line 110: mentions "fail-fast mode" (hyphenated) in prose context

**Impact**: Low. The spec does not explicitly state the serialized string for the `FailFast` variant. The Interface section does not document what text values are accepted for the backpressure mode field. A consumer could not determine from the spec alone whether to write `"fail_fast"` or `"fail-fast"` in YAML. This is a gap in the spec's Interface section — the accepted text values for enum-typed fields are not fully enumerated.

---

### 7. URL validation — edge case behavior not specified

**Classification**: `undocumented_behavior`

**Summary**: The spec states "A backend URL must be absolute and include a scheme; relative URLs or scheme-less values are rejected." The implementation uses two distinct checks: (1) `cannot_be_a_base()` to determine if the URL has a base, and (2) `scheme().is_empty()` for the edge case where a URL has a base but empty scheme. The spec does not mention this dual-path validation or the distinct error messages produced ("must be an absolute URL" vs. "must be an absolute URL with a scheme").

**Evidence**:
- `loader.rs` lines 129-138: Two branches with distinct error messages.
- The `cannot_be_a_base()` check and `scheme().is_empty()` check could theoretically produce different outcomes for pathological URL inputs.

**Impact**: Low. The spec correctly captures the contractual guarantee (absolute URL with scheme required). The two distinct error messages are an implementation detail. However, the spec could note that both "cannot be a base" and "empty scheme" are rejected paths.

---

## Summary Table

| # | Finding | Classification | Severity |
|---|---------|---------------|----------|
| 1 | `Server.tps_window_secs` missing from spec | `missing_interface` | Medium |
| 2 | Asymmetric default mechanism for `tps_window_secs` | `undocumented_behavior` | Low |
| 3 | `request_timeout` conflates HTTP client timeout with config default | `spec_error` | Low |
| 4 | `retry_after_base` accuracy confirmed | `spec_error` (informational) | None |
| 5 | `CompletionBias` defaults via struct, not loader | `undocumented_behavior` | Low |
| 6 | Backpressure mode enum serialization not enumerated | `undocumented_behavior` | Medium |
| 7 | URL validation edge case — dual error messages | `undocumented_behavior` | Low |

---

## Verdict

The spec is largely accurate in its description of configuration layering, validation invariants, and error behavior. The significant finding is **Finding 1** (`tps_window_secs` missing from spec), which represents an interface gap — a public configuration field that is configurable but undocumented. **Finding 6** also warrants attention as the accepted text values for enum fields are not fully specified.

No bugs were found in the implementation that contradict the spec's stated invariants.

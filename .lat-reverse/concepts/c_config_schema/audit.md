# Audit: c_config_schema — Configuration Schema

**Audited against:** `src/config/mod.rs`, `src/config/loader.rs`
**Spec version:** twice-corrected
**Status:** 3 findings

---

## Findings

### 1. `undocumented_behavior` — Uncovered configuration field: `Server.tps_window_secs`

The `Server` struct in `src/config/mod.rs` (line 325) exposes `tps_window_secs: u64` with a default of `10`. This field is absent from the spec entirely — not listed in "Configuration components" nor mentioned in any section. Consumers can set this parameter in configuration or via environment, but the spec does not declare it.

**Impact:** A consumer rewriting the configuration interface from spec alone would omit this field. The spec would be incomplete for reconstruction.

**Classification:** `undocumented_behavior`

---

### 2. `undocumented_behavior` — Environment variable prefix and separator not specified

The spec states: "Configuration is resolved from layered sources: file document, environment overrides, and built-in defaults." However, it does not specify:
- The environment variable prefix: `LLM_QDISC`
- The separator between nested keys: `__` (e.g., `LLM_QDISC__BACKEND__URL`)

The `config::Environment::with_prefix("LLM_QDISC").separator("__")` call in `loader.rs` (line 90) establishes the exact convention. Without this detail, a consumer cannot author environment variable overrides from the spec alone.

**Impact:** The resolution contract is incomplete — the environment override interface is described only partially.

**Classification:** `undocumented_behavior`

---

### 3. `undocumented_behavior` — `request_timeout` omitted from "Configuration components" listing

`request_timeout: Option<Duration>` is described under "Duration representation" (spec line 44) and "Validation guarantee" (spec line 54), but is not enumerated in the "Configuration components" section (spec lines 28–39), which lists every other configuration sub-component. The omission is minor since the interface is documented elsewhere, but the "Configuration components" section is presented as the exhaustive listing.

**Impact:** Low. The interface is documented; this is an organizational gap in the spec's enumeration.

**Classification:** `undocumented_behavior`

---

## "No How" Lint

**PASS.** The spec contains:
- No control flow descriptions
- No data structure details
- No function/method names used as concept identifiers
- No implementation-specific terminology

All interface statements are expressed as contractual guarantees.

---

## Verification Summary

| Spec Claim | Source Match | Verdict |
|---|---|---|
| Resolution order: env > file > defaults | `loader.rs` lines 65–90 | ✅ |
| `CONFIG_PATH` env var, default `config.yaml` | `loader.rs` line 63 | ✅ |
| Missing file is not an error | `.required(false)` line 88 | ✅ |
| YAML-only format | `FileFormat::Yaml` line 87 | ✅ |
| Invalid input fails entirely | `validate(&cfg)?` line 95 | ✅ |
| Backend defaults: `http://localhost:8000`, `1s` | `mod.rs` lines 46–55 | ✅ |
| Backend URL required at deserialization | No `#[serde(default)]` on `Backend.url` | ✅ |
| Algorithm: `fifo`, `wfq`, `drr`; default `drr` | `mod.rs` lines 172–178 | ✅ |
| Scheduler defaults: `max_active_flows=4`, `starvation_timeout=300s` | `mod.rs` lines 111–117 | ✅ |
| Completion bias: `enabled=true`, `target=0`, `predictive=false` | `mod.rs` lines 160–167 | ✅ |
| Flow defaults: `weight=1.0`, `priority=50` | `mod.rs` lines 189–206 | ✅ |
| Priorities: `interactive=100`, `agent=50`, `background=10` | `mod.rs` lines 219–241 | ✅ |
| Backpressure: mode `blocking`, `failfast`, `hybrid`; defaults correct | `mod.rs` lines 288–295, 262–285 | ✅ |
| Metrics endpoint default `/metrics` | `mod.rs` lines 304–316 | ✅ |
| Server bind default `0.0.0.0:8080` | `mod.rs` lines 328–344 | ✅ |
| KV policy defaults: `enabled=false`, `reject=0.95`, `delay=0.80` | `mod.rs` lines 84–91 | ✅ |
| Validation: all positive-threshold rules match | `loader.rs` lines 99–163 | ✅ |
| KV threshold ranges `(0,1]`, `[0,1]`, strict ordering | `loader.rs` lines 147–162 | ✅ |
| Backpressure mode token mismatch in error message noted | `loader.rs` line 117 (`fail_fast` vs `failfast`) | ✅ |
| `request_timeout` optional, default `None` | `mod.rs` line 29 | ✅ |
| Duration strings round-trip via humantime | `loader.rs` lines 10–54 | ✅ |
| `Server.tps_window_secs` present in source | **Absent in spec** | ❌ Finding #1 |
| Env prefix/separator convention | **Absent in spec** | ❌ Finding #2 |
| `request_timeout` in components listing | **Absent in components section** | ❌ Finding #3 |

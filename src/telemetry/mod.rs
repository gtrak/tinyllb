//! Structured logging and tracing initialization.
//!
//! Configures `tracing_subscriber` based on environment variables:
//!
//! - `RUST_LOG` — standard `tracing` filter directive.
//!   Default: `info,llm_qdisc_proxy=debug`.
//! - `LLM_QDISC_LOG_JSON=1` — switch from human-readable to JSON output
//!   (useful for shipping to a log aggregator such as Loki or Datadog).
//!
//! Span conventions are documented in
//! `docs/plans/001-llm-qdisc-proxy/TRACING.md`.

/// Initialize the global tracing subscriber.
///
/// Called once at the start of `main()`.  Configures:
/// - An env-filter for `RUST_LOG` (default `info,llm_qdisc_proxy=debug`).
/// - JSON output when `LLM_QDISC_LOG_JSON=1` is set; human-readable otherwise.
///
/// OpenTelemetry export is scaffolded as a commented-out `init_otlp()` stub
/// below — the codebase uses `tracing` spans throughout, so an OTLP exporter
/// can be wired up later without rewriting call sites.
pub fn init() {
    let json_mode = std::env::var("LLM_QDISC_LOG_JSON")
        .map(|v| v == "1")
        .unwrap_or(false);

    // Default filter: info globally, debug for our crate.
    let env_filter =
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info,llm_qdisc_proxy=debug".to_string());

    if json_mode {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .flatten_event(true)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }
}

/// Scaffold for OpenTelemetry OTLP export (scaffolded, not active).
///
/// To enable OTLP tracing, add these dependencies to Cargo.toml:
///
/// ```toml
/// opentelemetry = { version = "0.26", features = ["rt-tokio"] }
/// opentelemetry-otlp = { version = "0.26", features = ["tokio", "grpc-tonic"] }
/// opentelemetry_sdk = { version = "0.26", features = ["rt-tokio"] }
/// ```
///
/// Then uncomment this function and call it instead of `init()`:
///
/// ```rust,no_run
/// // use tracing_opentelemetry::layer as otel_layer;
/// // let otel = otel_layer()
/// //     .withExporter(opentelemetry_otlp::new_pipeline().trace()
/// //         .with_exporter(opentelemetry_otlp::new_exporter()
/// //             .tonic()
/// //             .with_endpoint(
/// //                 std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
/// //                     .unwrap_or_else(|_| "http://localhost:4317".to_string()),
/// //             )
/// //             .build()
/// //             .expect("OTLP exporter"),
/// //         ))
/// //     .build();
/// //
/// // tracing_subscriber::fmt()
/// //     .with_env_filter(std::env::var("RUST_LOG")
/// //         .unwrap_or_else(|_| "info,llm_qdisc_proxy=debug".to_string()))
/// //     .finish()
/// //     .with(otel)
/// //     .init();
/// ```
///
/// No code changes are required at span call sites — they already use
/// `tracing::info_span!` and `#[tracing::instrument]` which work with any
/// tracing layer including the OTLP exporter.
#[allow(dead_code)]
fn init_otlp() {
    // Placeholder — OTLP export is not active.
    // See the documentation above for instructions on enabling it.
    init();
}

//! Fairness benchmark: per-flow throughput distribution under WFQ.
//!
//! Drives a stub backend through the proxy with WFQ scheduling, measuring
//! per-flow completed work via the scheduler's service_done accessor.
//!
//! Three flows with weights 10:5:1 under sustained load. Uses a FIXED
//! BUDGET to measure service_done before all requests complete, ensuring
//! a genuine discriminator (a FIFO scheduler would produce equal service_done).
//!
//! USAGE: `cargo bench --bench fairness`
//!
//! The benchmark prints machine-parseable RESULT lines to stderr:
//!   RESULT fairness flow=FLOW_ID service_done=X weight=Y normalized_throughput=Z
//!   RESULT fairness_score=X ratio_A_C=Y ratio_A_B=Z

#[path = "stub_backend.rs"]
mod stub_backend;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;

use tinyllb::backend::BackendMonitor;
use tinyllb::config::{Algorithm, BackpressureMode, CompletionBias, KvPolicyConfig, Priorities, PriorityPolicy};
use tinyllb::flow::{FlowId, FlowRegistry};
use tinyllb::gateway;
use tinyllb::metrics;
use tinyllb::scheduler::Scheduler;
use stub_backend::{StubConfig, StubState};

/// Number of requests per flow.
const REQUESTS_PER_FLOW: usize = 20;
/// Stub service time.
const STUB_BASE_TIME_MS: u64 = 20;
const STUB_PENALTY: f64 = 0.05;

/// Flow weights.
const FLOW_A_WEIGHT: f64 = 10.0;
const FLOW_B_WEIGHT: f64 = 5.0;
const FLOW_C_WEIGHT: f64 = 1.0;

/// FIXED BUDGET for budget-limited measurement.
///
/// With max_active_flows=1 and 20ms stub, each admission cycle takes ~20ms.
/// With 20 requests per flow and weight ratio 10:1, WFQ should select
/// A ~10x more than C in the first ~100 cycles.
/// A budget of 200ms (~10 cycles) is enough to see discrimination.
const BENCHMARK_BUDGET_MS: u64 = 200;

/// Build a proxy app pointing at the given backend URL, with WFQ scheduling.
fn build_proxy_app(
    backend_url: &str,
    max_active_flows: u32,
) -> (Router, Arc<metrics::Metrics>, Arc<Scheduler>) {
    let m = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Arc::new(Scheduler::new(
        Algorithm::Wfq,
        max_active_flows,
        m.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        200,
        Duration::from_secs(60),
        Duration::from_secs(1),
        Duration::from_secs(300),
        CompletionBias {
            enabled: false,
            target_active_flows: 0,
            predictive_admit: false,
        },
        KvPolicyConfig::default(),
        Arc::new(BackendMonitor::empty()),
        PriorityPolicy::default(),
        Priorities::default(),
        tinyllb::config::KvBias::default(),
    ));

    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
        metrics: m.clone(),
        scheduler: scheduler.clone(),
        flow_registry,
        backpressure: tinyllb::config::Backpressure::default(),
        priorities: tinyllb::config::Priorities::default(),
        request_timeout: None,
        context: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let admin_router = tinyllb::api::create_router().with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(admin_router)
        .merge(gateway_router)
        .with_state(state);

    (app, m, scheduler)
}

/// Run a fairness benchmark: 3 flows, weights 10:5:1, measure service_done.
///
/// KEY: Uses BUDGET-LIMITED measurement. Does NOT wait for all requests to
/// complete. Instead, samples service_done at a fixed deadline. This ensures
/// A has significantly more service_done than C (WFQ discriminator), whereas
/// a drained-all scenario would produce equal service_done (trivial).
async fn run_fairness_bench() -> Duration {
    // Start stub backend.
    let stub_config = StubConfig {
        base_time_ms: STUB_BASE_TIME_MS,
        penalty: STUB_PENALTY,
        token_count: 10,
    };
    let stub_state = Arc::new(StubState::new(stub_config));
    let stub_addr = stub_backend::start_stub_backend(stub_state.clone()).await;
    let stub_url = format!("http://{}", stub_addr);

    // max_active_flows=1: single-slot competition.
    // This creates genuine admission-order discrimination.
    let (app, m, scheduler) = build_proxy_app(&stub_url, 1);

    // Register flows with weights 10, 5, 1.
    for (fid, weight) in &[
        ("A", FLOW_A_WEIGHT),
        ("B", FLOW_B_WEIGHT),
        ("C", FLOW_C_WEIGHT),
    ] {
        flow_registry_register(app.clone(), fid.to_string(), *weight, 50).await;
    }

    // Fire requests for all flows simultaneously.
    let body = r#"{"model":"bench","messages":[{"role":"user","content":"hi"}],"stream":true}"#
        .to_string();

    let mut handles = Vec::new();
    for _ in 0..REQUESTS_PER_FLOW {
        for (fid, _weight) in &[
            ("A", FLOW_A_WEIGHT),
            ("B", FLOW_B_WEIGHT),
            ("C", FLOW_C_WEIGHT),
        ] {
            let a = app.clone();
            let b = body.clone();
            let f = fid.to_string();
            handles.push(tokio::spawn(async move {
                let resp = a
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/v1/chat/completions")
                            .header("content-type", "application/json")
                            .header("x-llm-flow-id", &f)
                            .body(Body::from(b))
                            .unwrap(),
                    )
                    .await
                    .expect("request should succeed");
                assert_eq!(resp.status(), 200);
                let _bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                    .await
                    .expect("body should be readable");
            }));
        }
    }

    let wall_start = std::time::Instant::now();

    // BUDGET-LIMITED: sample service_done at fixed deadline, not wait for all.
    let budget = Duration::from_millis(BENCHMARK_BUDGET_MS);
    tokio::time::sleep(budget).await;

    // Read service_done for each flow at budget deadline.
    let sd_a = scheduler.service_done(&FlowId::new("A"));
    let sd_b = scheduler.service_done(&FlowId::new("B"));
    let sd_c = scheduler.service_done(&FlowId::new("C"));

    // Compute normalized throughput (service_done / weight).
    let norm_a = if FLOW_A_WEIGHT > 0.0 {
        sd_a / FLOW_A_WEIGHT
    } else {
        f64::INFINITY
    };
    let norm_b = if FLOW_B_WEIGHT > 0.0 {
        sd_b / FLOW_B_WEIGHT
    } else {
        f64::INFINITY
    };
    let norm_c = if FLOW_C_WEIGHT > 0.0 {
        sd_c / FLOW_C_WEIGHT
    } else {
        f64::INFINITY
    };

    // Fairness score: max/min ratio of normalized throughput.
    // Perfect fairness = 1.0. Higher = more unfair.
    // Under budget-limited measurement, A should have more service_done than C,
    // so A's normalized throughput should be lower (sd_a/10 vs sd_c/1).
    let norms = [norm_a, norm_b, norm_c];
    let max_norm = norms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min_norm = norms.iter().copied().fold(f64::INFINITY, f64::min);
    let fairness_score = if min_norm > 0.0 {
        max_norm / min_norm
    } else {
        f64::INFINITY
    };

    // Also compute raw ratios for the discriminator report.
    let ratio_a_c = if sd_c > 0.0 {
        sd_a / sd_c
    } else {
        f64::INFINITY
    };
    let ratio_a_b = if sd_b > 0.0 {
        sd_a / sd_b
    } else {
        f64::INFINITY
    };

    eprintln!(
        "RESULT fairness flow=A service_done={:.1} weight={:.1} normalized={:.1}",
        sd_a, FLOW_A_WEIGHT, norm_a
    );
    eprintln!(
        "RESULT fairness flow=B service_done={:.1} weight={:.1} normalized={:.1}",
        sd_b, FLOW_B_WEIGHT, norm_b
    );
    eprintln!(
        "RESULT fairness flow=C service_done={:.1} weight={:.1} normalized={:.1}",
        sd_c, FLOW_C_WEIGHT, norm_c
    );
    eprintln!(
        "RESULT fairness_score={:.3} max_norm={:.1} min_norm={:.1} wall={:?} active_flows={:.0} ratio_A_C={:.1} ratio_A_B={:.1}",
        fairness_score,
        max_norm,
        min_norm,
        wall_start.elapsed(),
        m.active_flows.get(),
        ratio_a_c,
        ratio_a_b
    );

    // Abort remaining tasks after budget deadline.
    for h in handles {
        h.abort();
    }
    // Give a moment for cleanup.
    tokio::time::sleep(Duration::from_millis(50)).await;

    wall_start.elapsed()
}

/// Register a flow via POST /flows (helper).
async fn flow_registry_register(app: Router, id: String, weight: f64, priority: u32) {
    let body = format!(
        r#"{{"id":"{}","weight":{},"priority":{}}}"#,
        id, weight, priority
    );
    let _resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/flows")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("register should succeed");
}

fn bench_fairness(c: &mut criterion::Criterion) {
    c.benchmark_group("fairness")
        .bench_function("wfq_10_5_1", |b| {
            b.to_async(tokio::runtime::Runtime::new().unwrap())
                .iter(|| async {
                    run_fairness_bench().await;
                });
        });
}

criterion::criterion_group!(
    name = benches;
    config = criterion::Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    targets = bench_fairness
);

criterion::criterion_main!(benches);

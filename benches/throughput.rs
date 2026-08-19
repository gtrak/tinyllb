//! Throughput benchmark: proxy with admission control vs direct uncontrolled concurrency.
//!
//! Uses a quadratic-concurrency-penalty stub to simulate GPU batching collapse:
//!   service_time = base_time * (1 + penalty * in_flight^2)
//!
//! The proxy should sustain higher aggregate throughput at high concurrency
//! because it limits concurrent backend requests via max_active_flows.
//!
//! Concurrency semantics: requests are dispatched in waves of `concurrency` size.
//! Each wave: spawn `concurrency` clients simultaneously, wait for all to complete,
//! then proceed to the next wave. This ensures `concurrency` clients are truly
//! simultaneous at the stub, so the peak in-flight equals the concurrency parameter.
//!
//! Criterion reports wall time; we compute tokens/sec as:
//!   tokens/sec = (num_requests * tokens_per_request) / wall_time

#[path = "stub_backend.rs"]
mod stub_backend;

use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use tinyllb::config::{Algorithm, Backpressure, BackpressureMode, Priorities};
use tinyllb::flow::FlowRegistry;
use tinyllb::gateway;
use tinyllb::metrics;
use tinyllb::scheduler::Scheduler;
use stub_backend::{StubConfig, StubState};

/// Number of token frames per request (matches stub config).
const TOKENS_PER_REQUEST: u32 = 10;
/// Total requests per benchmark run (spread across waves).
/// 32 is divisible by all concurrency levels (1, 4, 8, 16, 32).
const TOTAL_REQUESTS: usize = 32;

/// Stub parameters — quadratic model: base_time * (1 + penalty * in_flight^2)
///
/// These parameters make a PASS reachable because:
///
/// Proxy (capped at max_active_flows=4):
///   in-flight ≤ 4 → service_time ≤ 20ms * (1 + 0.05 * 16) = 20ms * 1.8 = 36ms
///   throughput = 4 / 36ms ≈ 111 req/s = 1110 tok/s
///
/// Direct at N=16:
///   in-flight = 16 → service_time = 20ms * (1 + 0.05 * 256) = 20ms * 13.8 = 276ms
///   throughput = 16 / 276ms ≈ 58 req/s = 580 tok/s
///   → Proxy (1110) > Direct (580): PASS
///
/// Direct at N=32:
///   in-flight = 32 → service_time = 20ms * (1 + 0.05 * 1024) = 20ms * 52.2 = 1044ms
///   throughput = 32 / 1044ms ≈ 31 req/s = 310 tok/s
///   → Proxy wins even more decisively.
///
/// Crossover point: where direct throughput ≈ proxy throughput
///   direct(N) = N / (20ms * (1 + 0.05 * N²)) req/s
///   proxy(4) = 4 / (20ms * 1.8) ≈ 111 req/s
///   N / (1 + 0.05*N²) ≈ 2.22
///   Solving: N ≈ 4.5 or N ≈ 10.4
///   For N ≥ 11, proxy clearly wins. Our test levels (16, 32) are well above this.
///
/// At low concurrency (N=1):
///   direct(1) = 1 / 20.1ms ≈ 50 req/s = 500 tok/s
///   proxy(1) = 1 / 20.1ms ≈ 50 req/s = 500 tok/s
///   → Tied (expected: no collapse at single request)
const STUB_BASE_TIME_MS: u64 = 20;
const STUB_PENALTY: f64 = 0.05; // quadratic coefficient

/// Build a proxy app pointing at the given backend URL, with a configurable
/// max_active_flows. Returns the Router and the metrics handle.
fn build_proxy_app(backend_url: &str, max_active_flows: u32) -> (Router, Arc<metrics::Metrics>) {
    let m = metrics::create_metrics();
    let flow_registry = Arc::new(FlowRegistry::new(1.0, 50));
    let scheduler = Scheduler::new_with_defaults(
        Algorithm::Fifo,
        max_active_flows,
        m.clone(),
        flow_registry.clone(),
        BackpressureMode::Blocking,
        100,
        Duration::from_secs(10),
        Duration::from_secs(1),
    );
    let state = gateway::AppState {
        client: gateway::build_client(),
        backend_url: Arc::new(url::Url::parse(backend_url).expect("valid backend URL")),
        metrics: m.clone(),
        scheduler: Arc::new(scheduler),
        flow_registry,
        backpressure: Backpressure::default(),
        priorities: Priorities::default(),
        request_timeout: None,
        retry_policy: tinyllb::config::RetryPolicy::default(),
    };

    let health_router = Router::new().route("/healthz", get(|| async { "ok" }));
    let gateway_router = gateway::create_router().with_state(state.clone());
    let metrics_router = Router::new()
        .route(
            "/metrics",
            get(tinyllb::metrics::endpoint::metrics_handler),
        )
        .with_state(state.clone());

    let app = Router::new()
        .merge(health_router)
        .merge(metrics_router)
        .merge(gateway_router)
        .with_state(state);

    (app, m)
}

/// Drive `total_requests` against a target URL, dispatched in waves of `wave_size`
/// concurrent clients each. Total waves = total_requests / wave_size.
///
/// Each wave: spawn `wave_size` clients simultaneously, wait for all to complete,
/// then proceed to the next wave. This ensures peak in-flight = wave_size.
///
/// Uses a **shared pooled reqwest client** (with keep-alive) so that connection
/// setup overhead does not stagger arrivals within a wave. This matches the proxy
/// path, which also uses a single pooled upstream client — making both paths
/// symmetric in connection handling.
async fn drive_concurrent_load(
    target_url: &str,
    client: reqwest::Client,
    wave_size: usize,
    num_waves: usize,
) -> Duration {
    let start = tokio::time::Instant::now();

    for _ in 0..num_waves {
        // Spawn `wave_size` clients simultaneously for this wave.
        let mut handles = Vec::with_capacity(wave_size);
        for _ in 0..wave_size {
            let client = client.clone();
            let url = target_url.to_string();
            handles.push(tokio::spawn(async move {
                let body =
                    r#"{"model":"bench","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
                let resp = client
                    .post(&url)
                    .header("content-type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .expect("request should succeed");
                // Drain the stream completely.
                let _bytes = resp.bytes().await.expect("response body readable");
            }));
        }

        // Wait for ALL clients in this wave to complete before the next wave.
        for handle in handles {
            handle.await.expect("client task should not panic");
        }
    }

    start.elapsed()
}

/// Run the benchmark for a given configuration.
///
/// # Arguments
/// * `scenario` - "direct" or "proxy"
/// * `concurrency` - number of simultaneous clients per wave
/// * `total_requests` - total number of requests (= concurrency * num_waves)
///
/// Prints a machine-parseable RESULT line to stderr with all metrics.
async fn run_bench(scenario: &str, concurrency: usize, total_requests: usize) -> Duration {
    // Shared stub state with quadratic penalty model.
    let stub_config = StubConfig {
        base_time_ms: STUB_BASE_TIME_MS,
        penalty: STUB_PENALTY,
        token_count: TOKENS_PER_REQUEST as usize,
    };
    let stub_state = Arc::new(StubState::new(stub_config));

    // Start stub backend.
    let stub_addr = stub_backend::start_stub_backend(stub_state.clone()).await;
    let stub_url = format!("http://{}", stub_addr);

    let num_waves = total_requests / concurrency;

    if scenario == "direct" {
        // Direct: clients talk to stub directly.
        // Use ONE pooled client (same as proxy path) for symmetric connection handling.
        let client = gateway::build_client();
        stub_state.reset();
        let wall_time = drive_concurrent_load(
            &format!("{}/v1/chat/completions", stub_url),
            client,
            concurrency,
            num_waves,
        )
        .await;

        let tokens = stub_state.tokens_emitted();
        let tok_per_sec = if wall_time.as_secs_f64() > 0.0 {
            (tokens as f64) / wall_time.as_secs_f64()
        } else {
            f64::INFINITY
        };
        let peak = stub_state.peak_in_flight();
        eprintln!(
            "RESULT direct concurrency={} waves={} requests={} tokens={} wall={:?} tok/s={:.1} peak_inflight={} base_time={}ms penalty={:.3}",
            concurrency,
            num_waves,
            total_requests,
            tokens,
            wall_time,
            tok_per_sec,
            peak,
            STUB_BASE_TIME_MS,
            STUB_PENALTY,
        );
        wall_time
    } else {
        // Proxy: clients talk to proxy, proxy talks to stub.
        let (proxy_app, _proxy_metrics) = build_proxy_app(&stub_url, 4);

        // Start proxy on ephemeral port.
        let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(proxy_listener, proxy_app).await.unwrap();
        });

        let proxy_url = format!("http://{}", proxy_addr);

        // Use ONE pooled client for symmetric connection handling.
        let client = gateway::build_client();
        stub_state.reset();
        let wall_time = drive_concurrent_load(
            &format!("{}/v1/chat/completions", proxy_url),
            client,
            concurrency,
            num_waves,
        )
        .await;

        let tokens = stub_state.tokens_emitted();
        let tok_per_sec = if wall_time.as_secs_f64() > 0.0 {
            (tokens as f64) / wall_time.as_secs_f64()
        } else {
            f64::INFINITY
        };
        let peak = stub_state.peak_in_flight();
        eprintln!(
            "RESULT proxy concurrency={} waves={} requests={} tokens={} wall={:?} tok/s={:.1} peak_inflight={} base_time={}ms penalty={:.3}",
            concurrency,
            num_waves,
            total_requests,
            tokens,
            wall_time,
            tok_per_sec,
            peak,
            STUB_BASE_TIME_MS,
            STUB_PENALTY,
        );
        wall_time
    }
}

fn bench_throughput(c: &mut criterion::Criterion) {
    let concurrency_levels = [1, 4, 8, 16, 32];

    for &concurrency in &concurrency_levels {
        let num_waves = TOTAL_REQUESTS / concurrency;
        let mut group = c.benchmark_group(format!(
            "throughput_concurrency_{}_waves_{}",
            concurrency, num_waves
        ));

        // Direct benchmark.
        group.bench_function(
            format!("direct_{}_concurrent_{}_waves", concurrency, num_waves),
            |b| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        run_bench("direct", concurrency, TOTAL_REQUESTS).await;
                    });
            },
        );

        // Proxy benchmark.
        group.bench_function(
            format!("proxy_{}_concurrent_{}_waves", concurrency, num_waves),
            |b| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        run_bench("proxy", concurrency, TOTAL_REQUESTS).await;
                    });
            },
        );

        group.finish();
    }
}

criterion::criterion_group!(
    name = benches;
    config = criterion::Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    targets = bench_throughput
);

criterion::criterion_main!(benches);

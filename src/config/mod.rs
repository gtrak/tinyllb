mod loader;

pub use loader::load;

use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

/// Top-level proxy configuration.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub backend: Backend,
    #[serde(default)]
    pub scheduler: Scheduler,
    #[serde(default)]
    pub flows: Flows,
    #[serde(default)]
    pub priorities: Priorities,
    #[serde(default)]
    pub backpressure: Backpressure,
    #[serde(default)]
    pub metrics: Metrics,
    #[serde(default)]
    pub server: Server,
    /// Optional request-level timeout. If set, the proxy cancels requests
    /// that exceed this duration. Applies to the forwarded request (connect +
    /// response body). Defaults to the reqwest client timeout (300s).
    #[serde(default, with = "loader::humantime_serde_option")]
    pub request_timeout: Option<Duration>,
    /// KV-cache-aware admission policy.  Defaults to `enabled: false`.
    #[serde(default)]
    pub kv_policy: KvPolicyConfig,
    /// Context-compression policy.  Defaults to `enabled: false`.
    #[serde(default)]
    pub context_policy: ContextPolicy,
}

/// Backend LLM service.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Backend {
    pub url: Url,
    #[serde(default, with = "loader::humantime_serde")]
    pub metrics_interval: Duration,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost:8000").unwrap(),
            metrics_interval: Self::default_metrics_interval(),
        }
    }
}

impl Backend {
    fn default_metrics_interval() -> Duration {
        Duration::from_secs(1)
    }
}

/// KV-cache-aware admission policy configuration.
///
/// When enabled, the proxy queries vLLM's `/metrics` endpoint for KV-cache
/// pressure and folds that into admission decisions alongside the flow-aware
/// scheduler.  Defaults to `enabled: false` so behavior is unchanged unless
/// explicitly configured.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KvPolicyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "KvPolicyConfig::default_reject_threshold")]
    pub reject_threshold: f64,
    #[serde(default = "KvPolicyConfig::default_delay_threshold")]
    pub delay_threshold: f64,
}

impl KvPolicyConfig {
    fn default_reject_threshold() -> f64 {
        0.95
    }

    fn default_delay_threshold() -> f64 {
        0.80
    }
}

impl Default for KvPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reject_threshold: Self::default_reject_threshold(),
            delay_threshold: Self::default_delay_threshold(),
        }
    }
}

/// Context-compression policy configuration.
///
/// When enabled, the proxy compresses conversation context by summarizing
/// older turns via a sidecar and persists transcripts to a SQLite store.
/// Defaults to `enabled: false` so behavior is unchanged unless explicitly
/// configured.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContextPolicy {
    #[serde(default = "ContextPolicy::default_enabled")]
    pub enabled: bool,
    #[serde(default = "ContextPolicy::default_compress_threshold")]
    pub compress_threshold: usize,
    #[serde(default = "ContextPolicy::default_head_keep_turns")]
    pub head_keep_turns: usize,
    #[serde(default = "ContextPolicy::default_live_keep_turns")]
    pub live_keep_turns: usize,
    #[serde(default = "ContextPolicy::default_compress_chunk_turns")]
    pub compress_chunk_turns: usize,
    #[serde(default = "ContextPolicy::default_summary_max_tokens")]
    pub summary_max_tokens: usize,
    #[serde(default = "ContextPolicy::default_store_path")]
    pub store_path: String,
    #[serde(default)]
    pub tokenizer_path: Option<String>,
    #[serde(
        default = "ContextPolicy::default_sidecar_request_timeout",
        with = "loader::humantime_serde"
    )]
    pub sidecar_request_timeout: Duration,
    #[serde(default = "ContextPolicy::default_compression_retries")]
    pub compression_retries: u32,
    #[serde(default)]
    pub prompt_template_path: Option<String>,
}

impl ContextPolicy {
    fn default_enabled() -> bool {
        false
    }

    fn default_compress_threshold() -> usize {
        100_000
    }

    fn default_head_keep_turns() -> usize {
        3
    }

    fn default_live_keep_turns() -> usize {
        6
    }

    fn default_compress_chunk_turns() -> usize {
        8
    }

    fn default_summary_max_tokens() -> usize {
        2048
    }

    fn default_store_path() -> String {
        "~/.local/share/llm-qdisc/transcripts.db".to_string()
    }

    fn default_sidecar_request_timeout() -> Duration {
        Duration::from_secs(60)
    }

    fn default_compression_retries() -> u32 {
        3
    }
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            compress_threshold: Self::default_compress_threshold(),
            head_keep_turns: Self::default_head_keep_turns(),
            live_keep_turns: Self::default_live_keep_turns(),
            compress_chunk_turns: Self::default_compress_chunk_turns(),
            summary_max_tokens: Self::default_summary_max_tokens(),
            store_path: Self::default_store_path(),
            tokenizer_path: None,
            sidecar_request_timeout: Self::default_sidecar_request_timeout(),
            compression_retries: Self::default_compression_retries(),
            prompt_template_path: None,
        }
    }
}

/// Scheduler configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Scheduler {
    #[serde(default)]
    pub algorithm: Algorithm,
    #[serde(default = "Scheduler::default_max_active_flows")]
    pub max_active_flows: u32,
    #[serde(
        default = "Scheduler::default_starvation_timeout",
        with = "loader::humantime_serde"
    )]
    pub starvation_timeout: Duration,
    #[serde(default)]
    pub completion_bias: CompletionBias,
}

impl Scheduler {
    fn default_max_active_flows() -> u32 {
        4
    }

    fn default_starvation_timeout() -> Duration {
        Duration::from_secs(300)
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            algorithm: Algorithm::default(),
            max_active_flows: Self::default_max_active_flows(),
            starvation_timeout: Self::default_starvation_timeout(),
            completion_bias: CompletionBias::default(),
        }
    }
}

/// Completion bias configuration.
///
/// When enabled, admission of requests for *new* flows (flows that do not
/// currently have an in-flight request) is deferred while the number of
/// active flows exceeds `target_active_flows`.  A value of `0` for
/// `target_active_flows` means "use `max_active_flows`".
///
/// When `predictive_admit` is true, the gate also checks per-flow progress:
/// if an active flow has delivered >= 90% of its estimated tokens, the gate
/// allows a new flow through (predictive admit) before the active flow finishes.
/// This is OFF by default.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompletionBias {
    #[serde(default = "CompletionBias::default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub target_active_flows: u32,
    /// Predictive admit: allow pre-admit when an active flow is near done
    /// (delivered >= 90% of estimated). OFF by default.
    #[serde(default)]
    pub predictive_admit: bool,
}

impl CompletionBias {
    fn default_enabled() -> bool {
        true
    }
}

impl Default for CompletionBias {
    fn default() -> Self {
        Self {
            enabled: true,
            target_active_flows: 0, // 0 = use max_active_flows
            predictive_admit: false,
        }
    }
}

/// Scheduling algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    Fifo,
    Wfq,
    #[default]
    Drr,
}

/// Per-flow defaults.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Flows {
    #[serde(default = "Flows::default_default_weight")]
    pub default_weight: f64,
    #[serde(default = "Flows::default_default_priority")]
    pub default_priority: u32,
}

impl Flows {
    fn default_default_weight() -> f64 {
        1.0
    }

    fn default_default_priority() -> u32 {
        50
    }
}

impl Default for Flows {
    fn default() -> Self {
        Self {
            default_weight: Self::default_default_weight(),
            default_priority: Self::default_default_priority(),
        }
    }
}

/// Priority class values.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Priorities {
    #[serde(default = "Priorities::default_interactive")]
    pub interactive: u32,
    #[serde(default = "Priorities::default_agent")]
    pub agent: u32,
    #[serde(default = "Priorities::default_background")]
    pub background: u32,
}

impl Priorities {
    fn default_interactive() -> u32 {
        100
    }

    fn default_agent() -> u32 {
        50
    }

    fn default_background() -> u32 {
        10
    }
}

impl Default for Priorities {
    fn default() -> Self {
        Self {
            interactive: Self::default_interactive(),
            agent: Self::default_agent(),
            background: Self::default_background(),
        }
    }
}

/// Backpressure configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Backpressure {
    #[serde(default)]
    pub mode: BackpressureMode,
    #[serde(default = "Backpressure::default_max_queue_depth")]
    pub max_queue_depth: u32,
    #[serde(
        default = "Backpressure::default_max_wait",
        with = "loader::humantime_serde"
    )]
    pub max_wait: Duration,
    #[serde(
        default = "Backpressure::default_retry_after_base",
        with = "loader::humantime_serde"
    )]
    pub retry_after_base: Duration,
}

impl Backpressure {
    fn default_max_queue_depth() -> u32 {
        100
    }

    fn default_max_wait() -> Duration {
        Duration::from_secs(10)
    }

    fn default_retry_after_base() -> Duration {
        Duration::from_secs(1)
    }
}

impl Default for Backpressure {
    fn default() -> Self {
        Self {
            mode: BackpressureMode::default(),
            max_queue_depth: Self::default_max_queue_depth(),
            max_wait: Self::default_max_wait(),
            retry_after_base: Self::default_retry_after_base(),
        }
    }
}

/// Backpressure mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackpressureMode {
    #[default]
    Blocking,
    FailFast,
    Hybrid,
}

/// Metrics endpoint configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Metrics {
    #[serde(default = "Metrics::default_endpoint")]
    pub endpoint: String,
}

impl Metrics {
    fn default_endpoint() -> String {
        "/metrics".to_string()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            endpoint: Self::default_endpoint(),
        }
    }
}

/// Server bind address.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Server {
    #[serde(default = "Server::default_bind")]
    pub bind: SocketAddr,
    /// Rolling window (seconds) used to smooth `llm_tokens_per_second`.
    #[serde(default = "Server::default_tps_window_secs")]
    pub tps_window_secs: u64,
}

impl Server {
    fn default_bind() -> SocketAddr {
        "0.0.0.0:8080".parse().unwrap()
    }

    fn default_tps_window_secs() -> u64 {
        10
    }
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: Self::default_bind(),
            tps_window_secs: Self::default_tps_window_secs(),
        }
    }
}

mod loader;

pub use loader::load;

use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

// @lat: [[config#Configuration Contract]]
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
    /// Premature-stop retry policy.  Defaults to `enabled: false`.
    #[serde(default)]
    pub retry_policy: RetryPolicy,
    /// Per-flow priority policy for the interactive-vs-batch heuristic.
    #[serde(default)]
    pub priority_policy: PriorityPolicy,
}

/// Backend LLM service.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Backend {
    pub url: Url,
    #[serde(default, with = "loader::humantime_serde")]
    pub metrics_interval: Duration,
    /// Inference-stall watchdog window. If the backend reports queued or
    /// running requests but neither `prompt_tokens_total` nor
    /// `generation_tokens_total` advances for this long, the engine is
    /// considered deadlocked and in-flight streams are aborted (dropping
    /// their backend connections) so they retry on fresh connections.
    /// 0 disables the watchdog.
    #[serde(default = "Backend::default_stall_timeout", with = "loader::humantime_serde")]
    pub stall_timeout: Duration,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost:8000").unwrap(),
            metrics_interval: Self::default_metrics_interval(),
            stall_timeout: Self::default_stall_timeout(),
        }
    }
}

impl Backend {
    fn default_metrics_interval() -> Duration {
        Duration::from_secs(1)
    }

    fn default_stall_timeout() -> Duration {
        Duration::from_secs(30)
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

/// Premature-stop retry policy configuration.
///
/// When enabled, the proxy re-sends `/v1/chat/completions` requests that
/// receive a degenerate stop (finish_reason: stop, no content, no tool_calls)
/// with bumped temperature.  Defaults to `enabled: false`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetryPolicy {
    #[serde(default = "RetryPolicy::default_enabled")]
    pub enabled: bool,
    #[serde(default = "RetryPolicy::default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "RetryPolicy::default_temperature_step")]
    pub temperature_step: f64,
    #[serde(default = "RetryPolicy::default_max_temperature")]
    pub max_temperature: f64,
    #[serde(default = "RetryPolicy::default_default_temperature")]
    pub default_temperature: f64,
}

impl RetryPolicy {
    fn default_enabled() -> bool {
        false
    }

    fn default_max_retries() -> u32 {
        2
    }

    fn default_temperature_step() -> f64 {
        0.3
    }

    fn default_max_temperature() -> f64 {
        1.5
    }

    fn default_default_temperature() -> f64 {
        0.0
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            max_retries: Self::default_max_retries(),
            temperature_step: Self::default_temperature_step(),
            max_temperature: Self::default_max_temperature(),
            default_temperature: Self::default_default_temperature(),
        }
    }
}


/// Priority classification policy for the turn-boundary state machine.
///
/// Tracks per-flow request cadence and assigns priority classes based on
/// turn-boundary detection.  Defaults to `enabled: true` with idle gap
/// threshold at 30s, suspected threshold at 5, and confirmed threshold at 12.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PriorityPolicy {
    #[serde(default = "PriorityPolicy::default_enabled")]
    pub enabled: bool,
    #[serde(
        default = "PriorityPolicy::default_idle_gap_threshold",
        with = "loader::humantime_serde"
    )]
    pub idle_gap_threshold: Duration,
    #[serde(default = "PriorityPolicy::default_agentic_suspected_threshold")]
    pub agentic_suspected_threshold: u32,
    #[serde(default = "PriorityPolicy::default_agentic_confirmed_threshold")]
    pub agentic_confirmed_threshold: u32,
}

impl PriorityPolicy {
    fn default_enabled() -> bool {
        true
    }

    fn default_idle_gap_threshold() -> Duration {
        Duration::from_secs(30)
    }

    fn default_agentic_suspected_threshold() -> u32 {
        5
    }

    fn default_agentic_confirmed_threshold() -> u32 {
        12
    }
}

impl Default for PriorityPolicy {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            idle_gap_threshold: Self::default_idle_gap_threshold(),
            agentic_suspected_threshold: Self::default_agentic_suspected_threshold(),
            agentic_confirmed_threshold: Self::default_agentic_confirmed_threshold(),
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
    #[serde(default)]
    pub kv_bias: KvBias,
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
            kv_bias: KvBias::default(),
        }
    }
}

/// KV-cache-aware selection bias.
///
/// Reorders selection among *eligible* waiting flows so that, under KV-cache
/// pressure, the flow holding the largest resident KV footprint is granted
/// the next permit — letting it finish and free blocks rather than being
/// preempted and paged into/out of CPU-offloaded KV cache. This is a
/// scheduling bias, NOT admission control: it never rejects or delays a
/// request, it only decides which eligible flow wins a permit.
// @lat: [[scheduler_policies#KV-Cache-Aware Selection Bias]]
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KvBias {
    #[serde(default = "KvBias::default_enabled")]
    pub enabled: bool,
    /// Fraction of KV cache above which the bias fully dominates selection.
    /// Below this, the bias scales the selection toward larger footprints in
    /// proportion to pressure. In [0,1].
    #[serde(default = "KvBias::default_bias_full_above")]
    pub bias_full_at: f64,
    /// Fraction of KV cache below which the bias is treated as pressure=0
    /// (falls back to pure fairness). In [0,1].
    #[serde(default = "KvBias::default_pressure_below")]
    pub pressure_below: f64,
}

impl KvBias {
    fn default_enabled() -> bool {
        true
    }

    fn default_bias_full_above() -> f64 {
        0.9
    }

    fn default_pressure_below() -> f64 {
        0.5
    }
}

impl Default for KvBias {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            bias_full_at: Self::default_bias_full_above(),
            pressure_below: Self::default_pressure_below(),
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

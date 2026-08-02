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
}

impl Server {
    fn default_bind() -> SocketAddr {
        "0.0.0.0:8080".parse().unwrap()
    }
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: Self::default_bind(),
        }
    }
}

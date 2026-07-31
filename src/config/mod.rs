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
}

/// Backend LLM service.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Backend {
    pub url: Url,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost:8000").unwrap(),
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
        }
    }
}

/// Scheduling algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    Fifo,
    #[default]
    Wfq,
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
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Backpressure {
    #[serde(default)]
    pub mode: BackpressureMode,
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

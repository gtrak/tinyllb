use std::time::Duration;

use crate::config::Config;

/// Serde helpers for Duration using human-readable strings.
pub mod humantime_serde {
    use super::*;

    /// Serializes a `Duration` as a human-readable string (e.g. "300s").
    pub fn serialize<S>(dur: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&humantime::format_duration(*dur).to_string())
    }

    /// Deserializes a human-readable duration string (e.g. "300s", "5m") into a `Duration`.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = <String as serde::Deserialize>::deserialize(deserializer)?;
        humantime::parse_duration(&s).map_err(serde::de::Error::custom)
    }
}

/// Serde helpers for `Option<Duration>` using human-readable strings.
pub mod humantime_serde_option {
    use super::*;

    /// Serializes an `Option<Duration>` as a human-readable string or null.
    pub fn serialize<S>(dur: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match dur {
            Some(d) => humantime_serde::serialize(d, serializer),
            None => serializer.serialize_none(),
        }
    }

    /// Deserializes an optional human-readable duration string into `Option<Duration>`.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let opt: Option<String> = serde::Deserialize::deserialize(deserializer)?;
        match opt {
            Some(s) => humantime::parse_duration(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

// @lat: [[config#Configuration Loading and Validation]]
/// Load configuration from YAML file and environment overrides.
///
/// Reads `$CONFIG_PATH` (defaults to `config.yaml`). If the file does not exist,
/// uses defaults. Then layers `TINYLLB__*` environment variable overrides.
/// Finally validates the resolved config.
pub fn load() -> anyhow::Result<Config> {
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());

    let builder = config::Config::builder()
        .set_default("backend.url", "http://localhost:8000")?
        .set_default("scheduler.max_active_flows", 4u32)?
        .set_default("scheduler.starvation_timeout", "300s")?
        .set_default("flows.default_weight", 1.0f64)?
        .set_default("flows.default_priority", 50u32)?
        .set_default("priorities.interactive", 100u32)?
        .set_default("priorities.agent", 50u32)?
        .set_default("priorities.background", 10u32)?
        .set_default("backpressure.mode", "blocking")?
        .set_default("backpressure.max_queue_depth", 100u32)?
        .set_default("backpressure.max_wait", "10s")?
        .set_default("backpressure.retry_after_base", "1s")?
        .set_default("backend.metrics_interval", "1s")?
        .set_default("metrics.endpoint", "/metrics")?
        .set_default("server.bind", "0.0.0.0:8080")?
        .set_default("kv_policy.enabled", false)?
        .set_default("kv_policy.reject_threshold", 0.95f64)?
        .set_default("kv_policy.delay_threshold", 0.80f64)?
        .set_default("retry_policy.enabled", false)?
        .set_default("retry_policy.max_retries", 2u64)?
        .set_default("retry_policy.temperature_step", 0.3f64)?
        .set_default("retry_policy.max_temperature", 1.5f64)?
        .set_default("retry_policy.default_temperature", 0.0f64)?
        .set_default("priority_policy.enabled", true)?
        .set_default("priority_policy.idle_gap_threshold", "30s")?
        .set_default("priority_policy.agentic_suspected_threshold", 5u32)?
        .set_default("priority_policy.agentic_confirmed_threshold", 12u32)?

        .add_source(
            config::File::from(std::path::PathBuf::from(&config_path))
                .format(config::FileFormat::Yaml)
                .required(false),
        )
        .add_source(config::Environment::with_prefix("TINYLLB").separator("__"));

    let settings = builder.build()?;
    let cfg: Config = settings.try_deserialize()?;

    validate(&cfg)?;
    Ok(cfg)
}

fn validate(cfg: &Config) -> anyhow::Result<()> {
    if cfg.scheduler.max_active_flows == 0 {
        return Err(anyhow::anyhow!("max_active_flows must be > 0"));
    }
    if cfg.scheduler.starvation_timeout.is_zero() {
        return Err(anyhow::anyhow!("starvation_timeout must be > 0s"));
    }
    if cfg.flows.default_weight <= 0.0 {
        return Err(anyhow::anyhow!("default_weight must be > 0"));
    }
    if matches!(
        cfg.backpressure.mode,
        super::BackpressureMode::FailFast | super::BackpressureMode::Hybrid
    ) && cfg.backpressure.max_queue_depth == 0
    {
        return Err(anyhow::anyhow!(
            "max_queue_depth must be > 0 when backpressure mode is {} or hybrid",
            match cfg.backpressure.mode {
                super::BackpressureMode::FailFast => "fail_fast",
                _ => "hybrid",
            }
        ));
    }
    if matches!(cfg.backpressure.mode, super::BackpressureMode::Hybrid)
        && cfg.backpressure.max_wait.is_zero()
    {
        return Err(anyhow::anyhow!(
            "max_wait must be > 0s when backpressure mode is hybrid"
        ));
    }
    if !cfg.backend.url.cannot_be_a_base() {
        // has a base — but check it's absolute (has scheme)
        if cfg.backend.url.scheme().is_empty() {
            return Err(anyhow::anyhow!(
                "backend.url must be an absolute URL with a scheme"
            ));
        }
    } else {
        return Err(anyhow::anyhow!("backend.url must be an absolute URL"));
    }

    // Validate metrics_interval.  A zero poll interval means the monitor never
    // polls, which silently disables the KV policy when enabled.
    if cfg.backend.metrics_interval.is_zero() {
        return Err(anyhow::anyhow!("backend.metrics_interval must be > 0s"));
    }

    // Validate KV policy thresholds.
    if cfg.kv_policy.enabled {
        if cfg.kv_policy.reject_threshold <= 0.0 || cfg.kv_policy.reject_threshold > 1.0 {
            return Err(anyhow::anyhow!(
                "kv_policy.reject_threshold must be in (0, 1]"
            ));
        }
        if cfg.kv_policy.delay_threshold < 0.0 || cfg.kv_policy.delay_threshold > 1.0 {
            return Err(anyhow::anyhow!(
                "kv_policy.delay_threshold must be in [0, 1]"
            ));
        }
        if cfg.kv_policy.delay_threshold >= cfg.kv_policy.reject_threshold {
            return Err(anyhow::anyhow!(
                "kv_policy.delay_threshold must be less than reject_threshold"
            ));
        }
    }

    // Validate retry policy constraints.
    let rp = &cfg.retry_policy;
    if rp.enabled {
        if rp.max_retries == 0 {
            return Err(anyhow::anyhow!(
                "retry_policy.max_retries must be > 0"
            ));
        }
        if rp.temperature_step <= 0.0 {
            return Err(anyhow::anyhow!(
                "retry_policy.temperature_step must be > 0.0"
            ));
        }
        if rp.max_temperature < rp.default_temperature {
            return Err(anyhow::anyhow!(
                "retry_policy.max_temperature must be >= default_temperature"
            ));
        }
        if rp.max_temperature > 2.0 {
            return Err(anyhow::anyhow!(
                "retry_policy.max_temperature must be <= 2.0 (OpenAI-compatible range)"
            ));
        }
    }

    // Validate priority-policy constraints.
    let pp = &cfg.priority_policy;
    if pp.agentic_confirmed_threshold <= pp.agentic_suspected_threshold {
        return Err(anyhow::anyhow!(
            "priority_policy.agentic_confirmed_threshold must be strictly greater than agentic_suspected_threshold"
        ));
    }
    if pp.agentic_suspected_threshold == 0 {
        return Err(anyhow::anyhow!(
            "priority_policy.agentic_suspected_threshold must be >= 1"
        ));
    }

    Ok(())
}

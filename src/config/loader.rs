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

/// Load configuration from YAML file and environment overrides.
///
/// Reads `$CONFIG_PATH` (defaults to `config.yaml`). If the file does not exist,
/// uses defaults. Then layers `LLM_QDISC__*` environment variable overrides.
/// Finally validates the resolved config.
pub fn load() -> anyhow::Result<Config> {
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());

    let builder = config::Config::builder()
        .set_default("backend.url", "http://localhost:8000")?
        .set_default("scheduler.algorithm", "wfq")?
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
        .set_default("metrics.endpoint", "/metrics")?
        .set_default("server.bind", "0.0.0.0:8080")?
        .add_source(
            config::File::from(std::path::PathBuf::from(&config_path))
                .format(config::FileFormat::Yaml)
                .required(false),
        )
        .add_source(config::Environment::with_prefix("LLM_QDISC").separator("__"));

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
    Ok(())
}

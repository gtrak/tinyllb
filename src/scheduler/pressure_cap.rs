//! KV-pressure-driven dynamic concurrency cap.
//!
//! Maps the backend's KV-usage pressure to an effective
//! `max_active_flows` ceiling. Soft cap: it only limits new admissions
//! (the DRR scheduler stops granting permits at the cap); in-flight
//! flows are never preempted. Disabled by default.

use std::sync::Arc;

use crate::backend::BackendMonitor;
use crate::config::KvPressure;

/// KV-pressure-driven dynamic concurrency cap handle, shared with the schedulers.
///
/// Holds the configuration and the backend monitor (pressure source).
// @lat: [[scheduler_policies#KV-Pressure Concurrency Cap]]
#[derive(Clone)]
pub struct PressureCapHandle {
    config: KvPressure,
    monitor: Arc<BackendMonitor>,
}

impl PressureCapHandle {
    /// Create a handle. `monitor` is used only as a pressure source; the
    /// cap never rejects — it only lowers the admission ceiling.
    pub fn new(config: KvPressure, monitor: Arc<BackendMonitor>) -> Self {
        Self { config, monitor }
    }

    /// Whether the cap is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Current global KV pressure in [0,1] from the backend monitor
    /// (same source as the KV bias and the admission gate).
    pub fn pressure(&self) -> f64 {
        self.monitor
            .snapshot()
            .map(|s| s.kv_usage.clamp(0.0, 1.0))
            .unwrap_or(0.0)
    }

    /// Effective active-flow ceiling for the given pressure.
    ///
    /// Pure: `min(max_active_flows, min over thresholds with
    /// pressure >= at of max_flows)`; `max_active_flows` when disabled
    /// or no threshold matches.
    pub fn effective_max(&self, max_active_flows: u32, pressure: f64) -> u32 {
        if !self.config.enabled {
            return max_active_flows;
        }
        self.config
            .thresholds
            .iter()
            .filter(|t| pressure >= t.at)
            .map(|t| t.max_flows)
            .min()
            .map(|m| m.min(max_active_flows))
            .unwrap_or(max_active_flows)
    }

    /// Convenience: read live pressure, return the effective cap.
    pub fn effective(&self, max_active_flows: u32) -> u32 {
        self.effective_max(max_active_flows, self.pressure())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KvPressureThreshold;

    fn handle(config: KvPressure) -> PressureCapHandle {
        PressureCapHandle::new(config, Arc::new(BackendMonitor::empty()))
    }

    fn kv_pressure(enabled: bool, thresholds: &[(f64, u32)]) -> KvPressure {
        KvPressure {
            enabled,
            thresholds: thresholds
                .iter()
                .map(|(at, max_flows)| KvPressureThreshold {
                    at: *at,
                    max_flows: *max_flows,
                })
                .collect(),
        }
    }

    #[test]
    fn effective_max_disabled_returns_max() {
        let h = handle(kv_pressure(false, &[(0.5, 3)]));
        assert_eq!(h.effective_max(4, 0.99), 4);
    }

    #[test]
    fn effective_max_empty_thresholds_returns_max() {
        let h = handle(kv_pressure(true, &[]));
        assert_eq!(h.effective_max(4, 0.99), 4);
    }

    #[test]
    fn effective_max_below_first_threshold_returns_max() {
        let h = handle(kv_pressure(true, &[(0.5, 3), (0.8, 2), (0.95, 1)]));
        assert_eq!(h.effective_max(4, 0.49), 4);
    }

    #[test]
    fn effective_max_bands() {
        let h = handle(kv_pressure(true, &[(0.5, 3), (0.8, 2), (0.95, 1)]));
        let max = 4u32;
        assert_eq!(h.effective_max(max, 0.5), 3);
        assert_eq!(h.effective_max(max, 0.79), 3);
        assert_eq!(h.effective_max(max, 0.8), 2);
        assert_eq!(h.effective_max(max, 0.949), 2);
        assert_eq!(h.effective_max(max, 0.95), 1);
        assert_eq!(h.effective_max(max, 1.0), 1);
    }

    #[test]
    fn effective_max_never_exceeds_max_active_flows() {
        let h = handle(kv_pressure(true, &[(0.1, 10)]));
        assert_eq!(h.effective_max(4, 0.5), 4);
    }

    #[test]
    fn pressure_clamped_and_absent_snapshot() {
        let h = handle(kv_pressure(true, &[(0.5, 3)]));
        assert_eq!(h.pressure(), 0.0);
    }
}

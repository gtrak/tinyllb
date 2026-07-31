// Queue-family metrics module.
//
// Metrics registered in `Metrics::new()` (src/metrics/mod.rs):
//
// - `llm_queue_depth` — gauge of current queue length (set by issue 05).
// - `llm_queue_wait_seconds` — histogram of queue wait durations (set by issue 05).
// - `llm_active_flows` — gauge of currently active flows (set by issue 08).
//
// At this issue (04) the values stay at their defaults until later
// issues populate them.

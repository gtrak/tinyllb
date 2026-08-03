// @lat: [[metrics#Metric Family Contracts]]
// Throughput-family metrics module.
//
// Metrics registered in `Metrics::new()` (src/metrics/mod.rs):
//
// - `llm_tokens_generated_total` — counter of total tokens generated
//   (incremented by the proxy after parsing backend responses).
// - `llm_tokens_per_second` — gauge updated every second by a background
//   task that computes the rate from the counter.

// @lat: [[metrics#Metric Family Contracts]]
// Backend-family metrics module.
//
// Metrics registered in `Metrics::new()` (src/metrics/mod.rs):
//
// - `vllm_requests_active` — gauge tracking in-flight requests to the
//   vLLM backend.  Incremented before forwarding, decremented on completion
//   (success or error).
// - `vllm_errors_total` — counter incremented on 5xx backend responses
//   and network errors.  4xx client errors are NOT counted.
// - `tinyllb_backend_retries_total` / `tinyllb_backend_retry_exhausted_total` —
//   transient backend error re-forward counters (retries issued / retries exhausted).

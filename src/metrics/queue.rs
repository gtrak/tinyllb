// @lat: [[metrics#Metric Family Contracts]]
// Queue-family metrics module.
//
// Metrics registered in `Metrics::new()` (src/metrics/mod.rs):
//
// - `llm_queue_depth` — gauge of current queue length. Set by the flow
//   scheduler: +1 when a request enters `admit()`, -1 when its admission
//   slot is acquired.
// - `llm_queue_wait_seconds` — histogram of queue wait durations. Observed by
//   the flow scheduler when the admission slot is acquired (wall clock from
//   entry to acquire).
// - `llm_active_flows` — gauge of currently active flows. Incremented when a
//   QueueTicket is created (permit acquired), decremented when the ticket is
//   dropped (release on success, error, or panic).

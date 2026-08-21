# Dynamic Concurrency Reduction Under KV Pressure

## Problem

Neither tinyllb nor llama-server reduces active slot concurrency when KV cache
pressure is high. This causes two bad outcomes:

1. **Wasted KV budget**: A slot at 93% KV still holds its slot while new
   requests queue behind it. The slot can't accept new work efficiently, and
   the queued requests can't start because the slot is occupied.

2. **kv_bias disables itself under pressure**: tinyllb's `kv_bias` (which
   prioritizes flows with the most resident KV so they finish first) shuts off
   when `pressure_below: 0.5` is exceeded. At high pressure, scheduling
   reverts to pure DRR round-robin — the exact opposite of what's needed.

## Current behavior

```
max_active_flows: 4 (static)
parallel: 4 (hard slot count)
kv_policy: admission gate only (reject/delay new requests)
kv_bias: active only when pressure < 50%
```

Flow:
1. tinyllb admits request → llama-server takes a slot
2. Slot fills KV → llama-server keeps it active (generating or stalled)
3. New request → llama-server may defer internally
4. tinyllb backpressure → new requests block at proxy layer

The slot stays occupied even when KV is full. No system reduces concurrency.

## Desired behavior

When KV pressure exceeds a threshold, tinyllb should lower `max_active_flows`
dynamically so fewer slots run concurrently, giving each active slot more KV
budget to finish its work.

Example:
- KV < 50%: max_active_flows = 4 (full concurrency)
- KV 50-80%: max_active_flows = 3
- KV > 80%: max_active_flows = 2
- KV > 95%: max_active_flows = 1 (one big request at a time)

This pairs with kv_bias: at high pressure, fewer slots + kv_bias = heavy
requests finish first, fresh requests wait.

## Required changes

### tinyllb

- Expose KV pressure as a metric from llama-server's `/metrics` endpoint
  (already available: `llamacpp:n_tokens_max` / context size ratio, or
  infer from slot utilization)
- Add `kv_pressure_thresholds` to config: list of (pressure, max_flows) pairs
- Scheduler reads KV pressure on each admit decision and adjusts
  `max_active_flows` dynamically
- kv_bias should remain active under high pressure (invert or remove
  `pressure_below` gate)

### llama-server (optional)

- Expose per-slot KV usage in `/slots` response (already partially there:
  `n_prompt_tokens` / `n_ctx`)
- Or expose aggregate KV utilization in `/metrics` for tinyllb to scrape

## Workaround

Set `max_active_flows: 2` statically. Limits throughput but ensures each slot
has enough KV budget for long-context requests.

## Context

Tested with Qwen3.8-27B on 3x RTX 5060 Ti. At ctx-size 180K with parallel 4,
a single 168K-token request filled 93% of KV, kv_bias disabled itself, and
the request completed but all KV was lost (no session resume in llama.cpp).
At ctx-size 340K, the same request only hits 49% pressure, keeping kv_bias
active — but new requests still queue behind it without concurrency reduction.

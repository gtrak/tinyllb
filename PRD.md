# PRD: Agent-Aware LLM Inference Scheduling Proxy

## 1. Product Overview

### Product Name

**LLM QDisc Proxy** (working name)

### Summary

An OpenAI API-compatible inference scheduling proxy designed for local LLM deployments. It sits between agentic applications and a vLLM backend, providing intelligent admission control, queue management, fairness policies, and workload shaping.

The proxy treats each agent, conversation, or workflow as a schedulable "flow" analogous to a network connection. It prevents GPU resource fragmentation caused by excessive concurrent generations and prioritizes completion of valuable long-running tasks.

### Problem Statement

Local LLM deployments increasingly serve multiple concurrent agentic workloads:

* coding agents
* research agents
* automation workflows
* chat sessions
* background jobs

Naive concurrency management causes:

* reduced tokens/sec due to excessive batching overhead
* KV cache pressure
* long-tail latency
* agent starvation
* partial progress across many tasks instead of completed tasks
* unstable throughput under load

Traditional network schedulers solved similar problems using queue disciplines (qdisc), traffic shaping, and flow scheduling. LLM inference needs an equivalent abstraction.

---

# 2. Goals

## Primary Goals

### G1. Maintain maximum inference throughput

The proxy should keep vLLM operating near optimal utilization by controlling concurrency.

Success criteria:

* Higher aggregate tokens/sec than uncontrolled concurrent requests.
* Fewer OOM/KV cache failures.
* Stable throughput under bursty workloads.

---

### G2. Prevent agent starvation

Long-running agent tasks should make measurable progress and complete.

Success criteria:

* No agent waits indefinitely.
* Configurable fairness policies.
* Existing generations are protected from excessive new arrivals.

---

### G3. Provide agent-aware scheduling

Requests should be grouped into logical flows.

Examples:

```
agent_id=coding-agent-1
conversation_id=abc123
workflow_id=build-and-test
```

Scheduling occurs at flow level rather than raw HTTP request level.

---

### G4. Maintain OpenAI API compatibility

Existing clients should require minimal or no changes.

Supported APIs:

```
POST /v1/chat/completions
POST /v1/completions
GET  /v1/models
```

The proxy should be deployable as:

```
client -> proxy -> vLLM
```

---

# 3. Non-Goals

The first version will not:

* replace vLLM's token scheduler
* perform model routing
* manage distributed inference clusters
* optimize CUDA kernels
* modify model execution
* implement user authentication
* provide UI dashboards

---

# 4. Architecture

## High-Level Design

```
                  Agent Clients

                       |
                       v

              OpenAI API Proxy Layer

                       |
                       v

             Scheduling Engine

        +--------------+--------------+
        |                             |
        v                             v

 Admission Controller          Queue Manager

        |
        v

   vLLM Backend

        |
        v

       GPU
```

---

# 5. Core Concepts

## Flow

A flow represents a logical workload.

Example:

```json
{
  "flow_id": "cursor-agent-123",
  "priority": 10,
  "tenant": "developer",
  "type": "interactive"
}
```

Flows own queues.

---

## Request

A single inference request.

Example:

```
POST /v1/chat/completions

flow_id=coding-agent
tokens=80000
max_tokens=8192
```

---

## Scheduling Unit

Initial implementation:

* request level

Future:

* token-level feedback
* generation continuation scheduling

---

# 6. Functional Requirements

# 6.1 OpenAI Compatible Gateway

## Requirements

The proxy must:

* accept OpenAI-compatible requests
* forward responses unchanged
* support streaming responses
* preserve error semantics

Example:

```
curl localhost:8000/v1/chat/completions
```

returns same format as vLLM.

---

# 6.2 Flow Identification

## Requirement

The proxy must determine workload identity.

Supported methods:

### Explicit header

```
X-LLM-Flow-ID: coding-agent
```

### Metadata field

```json
{
 "metadata": {
    "flow_id": "agent-123"
 }
}
```

### Default

Generate ephemeral flow.

---

# 6.3 Admission Control

The proxy decides whether a request enters vLLM.

Policies:

## Maximum active generations

Example:

```
max_active_flows=4
```

Requests beyond this queue.

---

## Token budget admission

Estimate:

```
prompt_tokens + max_output_tokens
```

Reject or delay requests exceeding capacity.

---

## KV-cache-aware admission

Optional future integration:

Input:

```
current KV usage
available blocks
```

Decision:

```
accept
delay
reject
```

---

# 6.4 Scheduling Algorithms

## MVP: FIFO with Flow Limits

Behavior:

```
flow A
  request1
  request2

flow B
  request1
```

Only limited flows execute simultaneously.

---

## V1: Weighted Fair Queueing

Each flow receives weight:

Example:

```
interactive: 10
coding: 5
background: 1
```

Scheduling:

```
token allocation ∝ weight
```

---

## V2: Deficit Round Robin

Maintain:

```
flow.credit += weight
```

Consume:

```
credit -= generated_tokens
```

Advantages:

* simple
* predictable
* handles variable workloads

---

# 6.5 Priority System

Flows have priority.

Example:

```
priority=100
human interactive

priority=50
agent execution

priority=10
background indexing
```

Rules:

* higher priority gets admission preference
* starvation protection prevents indefinite blocking

---

# 6.6 Completion Bias

Goal:

avoid:

```
10 agents @ 10%
```

prefer:

```
3 agents @ 90%
finish them
start next
```

Policy:

```
if active_generations > target:
    don't admit new flows
```

---

# 6.7 Backpressure

The proxy must communicate load.

Supported responses:

HTTP:

```
429 Too Many Requests
Retry-After
```

or:

queue indefinitely.

Modes:

```
blocking
fail-fast
hybrid
```

---

# 6.8 Streaming Support

Streaming responses must:

* preserve token streaming
* release scheduler resources on completion
* handle disconnects

Events:

```
request_started
token_received
request_completed
request_cancelled
```

---

# 6.9 Cancellation

Support:

* client disconnect
* timeout
* explicit cancel

Resources:

```
queue entry removed
flow credit restored
```

---

# 7. Configuration

Example:

```yaml
backend:
  url: http://localhost:8000

scheduler:
  algorithm: drr

  max_active_flows: 4

  starvation_timeout: 300s

flows:
  default_weight: 1

priorities:
  interactive: 100
  agent: 50
  background: 10
```

---

# 8. Observability

## Metrics

Prometheus compatible.

### Queue

```
llm_queue_depth
llm_queue_wait_seconds
llm_active_flows
```

---

### Throughput

```
llm_tokens_generated_total
llm_tokens_per_second
```

---

### Scheduling

```
llm_flow_credit
llm_flow_starvation_seconds
```

---

### Backend

```
vllm_requests_active
vllm_errors_total
```

---

# 9. API Extensions

## Flow Registration API

Optional.

```
POST /flows
```

Example:

```json
{
"id":"agent1",
"weight":5,
"priority":50
}
```

---

## Queue Status

```
GET /queue
```

Response:

```json
{
"active":4,
"waiting":12,
"flows":[
 {
  "id":"coder",
  "position":2
 }
]
}
```

---

# 10. Deployment

## Local Single GPU

```
agent
 |
proxy
 |
vllm
 |
GPU
```

---

## Multi-GPU Local

Example:

```
proxy
 |
vllm TP=2
 |
5060Ti pair
```

---

# 11. Technology Choice

## Recommended MVP

Rust:

Advantages:

* async networking
* low overhead
* good long-running service model
* integrates well with existing infrastructure

Stack:

```
axum
tokio
serde
tower
prometheus
```

---

Alternative:

Python:

Advantages:

* faster prototype
* easier OpenAI compatibility

Disadvantages:

* less attractive for a permanent daemon

---

# 12. MVP Scope

Estimated effort:

## Phase 1: Basic Queue Proxy

Features:

* OpenAI API forwarding
* streaming
* max concurrency
* FIFO queue
* metrics

Estimate:

1-2 weeks

---

## Phase 2: Agent Scheduling

Features:

* flow IDs
* weighted scheduling
* priorities
* starvation protection

Estimate:

2-4 weeks

---

## Phase 3: vLLM Integration

Features:

* KV awareness
* dynamic admission
* token feedback

Estimate:

4-8 weeks

---

# 13. Future Features

## Model Router

Route:

```
simple tasks -> small model
complex tasks -> large model
```

---

## Speculative Workload Management

Detect:

```
agent spawned 20 subtasks
```

Apply:

```
budget controls
```

---

## Persistent Queues

Survive:

* backend restart
* GPU reset
* model reload

---

## Agent Economics

Track:

```
tokens spent
time consumed
success rate
```

Use for scheduling.

---

# 14. Success Metrics

The proxy is successful if:

| Metric                   | Target                           |
| ------------------------ | -------------------------------- |
| Aggregate throughput     | +20% vs uncontrolled concurrency |
| GPU utilization variance | reduced                          |
| OOM failures             | near zero                        |
| Agent completion latency | improved                         |
| Starvation events        | zero                             |
| Queue visibility         | complete                         |

---

# 15. Positioning

This product is essentially:

**"tc/qdisc for LLM inference workloads."**

The core abstraction:

```
network packet
        ->
LLM request/token

connection
        ->
agent flow

qdisc
        ->
scheduler

bandwidth
        ->
GPU decode capacity
```

The most valuable differentiator is not another OpenAI proxy. It is **resource scheduling semantics for autonomous agents running against constrained local inference hardware.**


# [S] moq-uring: add opt-in NAPI busy polling

## Goal

Implement and verify the behavior tracked in [#3203](https://github.com/moq-dev/moq/issues/3203)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to #2875.

The relay already pins one worker and one ring per core. io\_uring NAPI busy polling can keep a worker close to the NIC receive path and reduce wakeup latency, at the cost of continuously consuming CPU and power while it polls.

#### Proposal

- Add an explicit worker-level NAPI configuration with disabled as the default.
- Start with dynamic NAPI-ID tracking through `register_napi`. Add static IDs only if deployment can discover and maintain the correct queue IDs reliably.
- Make busy-poll duration and preferred-busy-poll behavior observable in relay configuration and stats.
- Report unsupported registration clearly. Do not silently claim the mode is active.
- Ensure unregister and worker teardown are safe.
- Document that this mode is for dedicated, latency-sensitive cores, not general self-hosting.

#### Acceptance

Measure on bare metal with the deployment NIC and queue affinity configured, not loopback. Compare disabled and several busy-poll durations under low, medium, and saturated load. Record p50, p99, and p999 packet latency, relay CPU, CPU idle residency, interrupts, drops, goodput, and power if available. Ship only as opt-in unless fleet-level data shows an acceptable idle-cost tradeoff.

## Closes

- [#3203](https://github.com/moq-dev/moq/issues/3203) - close this issue when the quest finishes

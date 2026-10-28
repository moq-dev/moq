# [L] Attribute UDP queue loss before tuning buffers

## Goal

Relay socket-buffer settings follow measured loss, queue delay and memory
cost across useful load, with local starvation separated from packet drops.

## Plan

[Socket binding](/rs/moq-sock/src/bind.rs) requests 8 MiB receive and send
buffers and already reads back OS clamping. These are ceilings, not eager
allocations. The io_uring `rx_enobufs` metric counts provided-buffer
starvation, not confirmed packet loss. Neither UDP receive stack currently
parses `SO_RXQ_OVFL` for socket-drop attribution.

Extend benchmark instrumentation to record granted queue sizes and actual
local drops. Linux [socket documentation](https://man7.org/linux/man-pages/man7/socket.7.html)
describes `SO_RXQ_OVFL`; account for ancillary space, counter rollover, GRO,
and the fact that missing socket-drop evidence cannot locate every loss.
Keep instrumentation private unless a production consumer needs it.

Sweep workers × offered flows and publishers × subscribers, with kernel
queue size and userspace pool depth varied independently. Include CPU
contention, bursts and rate-limited deep finite queues with overflow drops.
Report useful goodput, latency, CPU, memory and local versus path loss. Check
both shared and io_uring receive paths. Retain fixed defaults if measurements
do not justify a new configuration surface; larger queues can worsen delay.

Coordinate with [io_uring buffer pool memory](/quest/m2/perf/uring-pool-memory.md) and
[runtime selection](/quest/m2/perf/shard-default.md), sharing fixtures and
reports without making independent experiments block each other.

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. Report actual API, configuration and wire impact.

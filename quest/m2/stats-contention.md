# [M] Measure shared traffic-counter contention during fanout

## Goal

Quantify the CPU cost of enabled traffic accounting across workers, and reduce it
only if measurements justify a change while preserving exact accounting semantics.

## Plan

In `rs/moq-net/src/stats.rs`, `BroadcastEntry::tier` shares one `Arc<TierCounters>`
per broadcast/tier. `Meter::bytes`, `frames`, `group`, and `datagram` update relaxed
atomics on those shared counters. Many viewers of the same broadcast can update
the same words from different workers. Relaxed ordering still requires cache-line
ownership; whether this materially limits relay fanout needs measurement.
The existing group microbenchmarks use untagged model handles and do not establish
this cost. This is distinct from the cache pool's global recency counter.

- Benchmark accounting disabled versus enabled at the same fixed delivered load.
  Sweep 1/2/4/8 workers, same-broadcast versus distinct-broadcast traffic, realistic
  audio/video chunk sizes, and stream versus datagram delivery. Include normal
  and frequent metrics scrapes to expose aggregation cost.
- Add a focused tagged-model benchmark and run the relay fanout workload. Report
  cycles/instructions per delivered byte/frame, throughput, p99 latency, and the
  load generator's headroom. Separate same-word contention from false sharing.
- On supported Linux hardware, use perf c2c to locate contended cache lines;
  ordinary CPU profiles and scaling curves remain useful where PMU support is
  absent. Do not report unsupported counters as zero.
- Compare worker/session-local aggregation or bounded batching before assuming
  padding is sufficient: padding cannot remove contention on the same atomic.
  Avoid adding a thread-local lookup or global lock to every payload operation.
- Preserve monotonic snapshots, tier/path attribution, exact final totals, stale
  and datagram accounting, and lifecycle gauges on abort, cancellation, handoff,
  and last-drop. Bound delayed visibility explicitly if batching is chosen, and
  bound shard memory when sessions or broadcasts churn.
- Retain a change only with repeatable wins and no material single-worker or
  disabled-accounting regression. A measured negligible cost closes this quest
  without changing production counters.

Reference: [Linux cache-line contention analysis](https://www.kernel.org/doc/html/latest/kernel-hacking/false-sharing.html).

## Related

- [Session micro-costs](/quest/m1/perf/session-micro.md) - owns ingress stats batching; this quest measures cross-worker contention and egress fanout
- [Cache shard](/quest/m1/perf/cache-shard.md) - separate shared recency-counter bottleneck
- [Relay profiling](/quest/m2/performance-profiles.md) - reproducible CPU attribution

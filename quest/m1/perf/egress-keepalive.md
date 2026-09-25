# [S] Measure and reduce egress cache-refresh overhead

## Goal

Reduce measured cache-refresh overhead during batched delivery without
expiring a group while a slow subscriber or FETCH reader is draining it.
Retain the current behavior if the improvement is within measurement noise.

## Plan

`group::Consumer::keep_alive` takes a state read guard and calls
`Charge::refresh`. Both lite and IETF publishers call it between completed
frame writes. This protects a drain that spans longer than `latency_max`;
a stamp only at batch fill is insufficient. `Charge::touch` already skips
population accounting when the coarse timestamp has not advanced.

Measure read-guard, clock, and atomic costs separately for fast fanout and
flow-controlled readers, including SUBSCRIBE and FETCH. Preserve per-frame
liveness refresh unless an alternative proves the same retention behavior.
Do not introduce another clock or pool-accounting mechanism: those belong to
[Cache shard](/quest/m1/perf/cache-shard.md). Recheck its implementation
before optimizing the remaining publisher overhead.

Extend the existing group and track Criterion targets and add a bounded
session regression to normal CI. Keep
`slow_batch_reader_survives_expiry_with_keep_alive`, and cover expiry scans
while a batch drains, cancellation, and eventual expiry after reads stop.
Report delivered bytes, refresh cost, CPU, and throughput for paired runs;
fewer refresh calls alone are not evidence of a win.

## Related

- [Cache shard](/quest/m1/perf/cache-shard.md) - owns shared accounting and clock costs
- [Ingest batch](/quest/m1/perf/ingest-batch.md) - ingest batching
- [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - runtime clock costs

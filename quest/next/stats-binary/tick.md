# [M] Allocation-free stats tick

## Goal

In steady state, a `moq-stats` producer tick allocates nothing per entry: the
moq-net registry refills a caller-owned report, and the producer buckets,
diffs, and encodes from reused buffers. A benchmark swept over broadcasts x
tiers (and nodes for the aggregate) shows allocations per tick flat in the
table size, and the JSON flavors get cheaper too.

## Plan

- Break `moq_net::stats::Registry::report()` on dev into
  `report(&self, &mut Report)`, which clears and refills the report's
  collections while keeping their capacity. There is no second method and no
  shim. Update every caller (relay, moq-stats, tests).
- Work out where today's tick allocates: the per-group `HashMap`/`HashSet`
  bucketing in `produce.rs`, `String` clones of paths and track names, the
  per-track `TrafficFrame` maps. Make them reuse state held across ticks. The
  JSON frame only needs building while a JSON track is subscribed. Churn
  inside moq-json belongs to [JSON churn](/quest/next/json-churn.md).
- Some allocations are unavoidable: each published frame becomes an owned
  `Bytes` the track caches, and `moq_flate::Encoder::frame()` returns a fresh
  one. Reduce this to one allocation per published frame, not per entry. If
  that needs a buffer-reusing flate API, add it here and keep it in step with
  the [flate track wrapper](/quest/future/flate/track.md).
- Count allocations in the benchmark with a counting global allocator, and
  report before and after in the PR. The benchmark covers the `.fb.z`-ready
  path (registry to encoder input), not the moq-json encoders.

Public API impact: breaking on moq-net (`Registry::report`); lands on dev.
Wire impact: none.

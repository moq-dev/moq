# [M] Measure shared cache backing amplification

## Goal

Small retained media samples do not pin disproportionately large backing
allocations when a measured alternative earns its copying cost.

## Plan

[Group storage](/rs/moq-net/src/model/group.rs) bills `Bytes::len()` and stores
the supplied view. [fMP4 import](/rs/moq-mux/src/container/fmp4/import.rs)
slices samples from a fragment. Selective expiry across groups or tracks can
leave a small sample holding a large fragment after most logical charges
disappear. Cache capacity is not a promised hard RSS limit; this is a backing
amplification hypothesis, not proof of a broken capacity contract.

Benchmark the actual importer and cache, sweeping fragment size × retained
sample fraction × tracks. Include mixed audio/video, several keyframe groups
in a fragment, selective expiry and independently allocated samples as a
control. Measure uniquely owned backing bytes versus `Pool::used()`, copy
volume, allocations, CPU and delivery latency. Do not double count shared
backing or assume each slice owns a fresh allocation. Compare continued
sharing with copying/compaction only where the workload warrants it.

This covers completed reliable groups. Coordinate with
receive leases ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878))
and [browser media copies](/quest/m2/performance-survey/browser-media-copies.md).
Decoded datagrams are already billed to the cache pool
([moq-dev/moq#6413](https://github.com/moq-dev/moq/pull/6413)) and the media
reorder queues are bounded ([moq-dev/moq#6453](https://github.com/moq-dev/moq/pull/6453)).

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. No API or wire change is expected; report actual impact.

# [M] Cache metadata follows live occupancy

## Goal

Tracks that expire a large cache do not retain unjustified bookkeeping
capacity after their payload charges fall.

## Plan

[Track expiry](/rs/moq-net/src/model/track.rs) pops or retains entries in
`arrival` and `evict` without shrinking backing storage. The `CACHE_OVERHEAD`
rationale assumes a deque holds at most twice its current entries; draining
does not preserve that assumption. Long-lived relay tracks can therefore
retain historical queue capacity after `Pool::used()` falls.

Sweep tracks × peak cached groups, expire to the protected live group, then
refill. Record queue capacities, logical charges, live allocation bytes and
RSS separately. Use simulated time for expiry correctness and repeated
grow/drain cycles to expose reclamation CPU, copies and allocation churn.
Preserve arrival cursors, live-edge protection and useful cache retention.
Correct the accounting explanation with the chosen implementation.

Front eviction ([moq-dev/moq#6993](https://github.com/moq-dev/moq/pull/6993))
kept the group's `kio::log` representation and sheds by advancing its base. Land the benchmark now, but any shrink of the
`arrival`/`evict` deques waits on
[log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md)'s verdict,
since that quest may move `TrackState` arrival order onto the kio log.
Idle GC clock and lock work was settled by
[moq-dev/moq#6291](https://github.com/moq-dev/moq/pull/6291); this owns
retained queue backing.

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. No API or wire change is expected; report actual impact.

## Related

- [Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md) - may replace the `arrival` deque this quest would shrink

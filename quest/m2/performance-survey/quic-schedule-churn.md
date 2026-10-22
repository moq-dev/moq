# [M] QUIC priority changes touch only relevant scheduling state

## Goal

Changing or retiring queued scheduling groups does not impose unnecessary
work on unrelated groups sharing a connection.

## Plan

[The QUIC scheduler](/rs/moq-quic/src/connection/streams/schedule.rs)
linearly searches round-robin VecDeques in `unring_group` and retirement
(lines 786-821). Priority changes call that path. On shared relay sessions,
repeated updates or unfavorable retirement order can multiply changed groups
by unrelated queued groups. Existing dispatch/stream-churn benches do not
isolate scheduling-group reprioritization and retirement.

Sweep queued groups × changed/retired groups, and domains × retired domains.
Measure front/middle/tail and mixed-order operations, priority-update CPU,
allocations and packet-dispatch tail latency during churn. Confirm through
publisher × subscriber traffic on a shared session. Preserve byte fairness,
priority ordering and eventual progress rather than changing scheduler policy.

Benchmark the priority semantics from
[moq-dev/moq#6142](https://github.com/moq-dev/moq/pull/6142). No new
priority contract is selected by this performance quest.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.

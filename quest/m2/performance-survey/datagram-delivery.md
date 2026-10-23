# [M] Benchmark complete datagram delivery fan-out

## Goal

Session datagram costs are measured across independent publishers and
viewers, and any demonstrated avoidable per-object work is removed.

## Plan

The [lite publisher](/rs/moq-net/src/lite/publisher.rs) serializes and
schedules each delivered datagram in `serve_datagram`; the
[IETF adapter](/rs/moq-net/src/ietf/datagram.rs) has its own framing path.
Model-only datagram benches cannot attribute complete adapter delivery.
No regression is claimed merely because framing happens per subscriber.

Extend session benchmarks with publishers × viewers, payload sizes and
object cadence, covering supported lite and IETF datagram paths. Measure
CPU/delivered object, allocations/copies and queue residence, with complete
sequence/payload validation. Separate the in-memory protocol cost from real
QUIC scheduling and congestion. Under bufferbloat record offered, received,
dropped and expired objects and slow-viewer isolation; best-effort loss must
not make lower CPU look like a gain.

Keep framing, priority, expiry and memory-accounting behavior intact. The
[release audit](https://github.com/moq-dev/moq/pull/6311) owns the decoded
memory-accounting defect. This quest owns the recurring audit's missing
session-level datagram measurement and any independent measured optimization.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.

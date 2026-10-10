# [XS] Pin retransmits to stream priority

## Goal

A `moq-quic` regression test proves that a lost range on a lower-priority
stream waits behind a higher-priority stream's new data, so the
[scheduler](/quest/m1/quic/scheduler.md) rewrite has to keep that
ordering.

## Plan

Decided 2026-10-10 with the maintainer: this already holds, so the quest
only pins it before the XL scheduler rewrite. `StreamsState::retransmit`
(`rs/moq-quic/src/connection/streams/state.rs`) re-queues the lost range's
stream at that stream's own priority, and `write_stream_frames` always pops
the highest-priority pending stream. Buffered data is already within flow
control, so priority decides. Within one stream, its retransmits still go
before its new data.

Add a unit test beside the existing `StreamsState` tests in `state.rs`: send
on a low-priority stream, mark its range lost through `retransmit`, write
new data on a high-priority stream, and assert that `write_stream_frames`
emits the high-priority stream first and the retransmit after it.

Public API: none. Wire: none.

## Related

- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - keeps
  this test passing and adds the fair-tier retransmit test

# [S] moq-quic's idle connections keep no burst-sized state

## Goal

A moq-quic connection's per-stream "readable" event queue stays bounded by the
number of streams with unread data, not by the size of a burst, and a quiet
connection keeps no per-stream bytes for streams it no longer has.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7528](https://github.com/moq-dev/moq/pull/7528), which
stopped recycling freed receive state.

- Deduplicate the per-stream readable events the connection queues; today the
  queue can grow to the size of a burst.
- Trace the roughly 70 to 90 bytes per peak concurrent stream that a quiet
  connection still keeps after #7528. The stream maps holding their peak
  capacity are the likely part; release or shrink them at rest.
- Measure retained bytes after a burst with #7528's `receive_bursts` bench.

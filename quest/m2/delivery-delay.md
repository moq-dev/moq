# [S] Per-frame delivery delay histogram

## Goal

The relay's `moq-stats` egress side reports, per broadcast, a byte-weighted
cumulative histogram of per-frame delivery delay (from when a frame is
written to when the peer acknowledges it), so viewer jitter is visible beside
the `lag` histogram. Subscriptions on a backend that cannot see
acknowledgements leave it absent, never zero.

## Plan

Decided 2026-10-26 (maintainer, on
[moq-dev/moq#6791](https://github.com/moq-dev/moq/pull/6791)): this quest's
frontier half, moving the acknowledged frontier at every frame end and
counting `dropped` per frame, moved into the
[viewer lag histogram](/quest/m2/qos/lag-histogram.md), which no longer
lands at group granularity. That quest already awaits each frame's
acknowledgement through `poll_acked`; this one adds the delay it observes.

Record `acked_at - written_at` per frame, where `acked_at` is when the serve
task is woken by `poll_acked`. Its low edge approaches the path RTT and its
spread is the jitter. Use the `lag` histogram's type and bucket contract.
Document that the value still includes the return one-way delay and the
peer's delayed-ACK timer, since the hook reports no corrected instant
(decided 2026-10-17 on #5817).

Tests: delivery delay staying inside one bucket of the true RTT plus the
peer's `max_ack_delay`, and the histogram absent over a backend without the
hook.

Public API: an additive `Traffic` field. Wire: an additive stats JSON field.

## Required

- [Viewer lag histogram](/quest/m2/qos/lag-histogram.md) - the per-frame
  acknowledgement tracking and histogram type this reuses

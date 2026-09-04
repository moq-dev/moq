# [S] Starvation at frame granularity

## Goal

The starvation histogram and dropped-media counters are sampled at every
frame boundary instead of once per group, and a second histogram reports
per-frame delivery delay so viewer jitter is visible per broadcast. The wire
shape from the group slice is unchanged apart from the new histogram.

## Plan

A frame is atomic to the viewer: it is useful only once fully delivered. At
each frame end the per-group serve task already knows the stream offset (from
the `Writer` counter) and the frame timestamp. Record `(offset, timestamp,
bytes, written_at)` and await `poll_acked(offset)` from the released
`web-transport-trait` hook, interleaved with the writes of later frames so a
lagging ACK never stalls sending. One waiter per stream suffices: offsets are
acknowledged in order for the purpose of this metric, so poll the oldest
pending frame and retire everything at or below the acknowledged offset.

At each acknowledged frame, add the frame's bytes to the lag histogram at
`newest produced timestamp - this frame's timestamp`. When a stream is reset
before a frame is acknowledged, `dropped_duration` now grows by the span from
the newest acknowledged frame to the newest written one, which is the exact
media the viewer lost.

Add a second byte-weighted cumulative histogram of delivery delay:
`acked.received - written_at` per frame, where `received` is the
ACK-delay-corrected instant the hook returns. Its low edge approaches the
path RTT and its spread is the jitter. Document that the value still includes
the return one-way delay and that a backend without the hook leaves the
histogram absent, never zero.

Backends that return unsupported from `poll_acked` keep the group-granularity
sampling from the parent quest, so the histogram never disappears when the
relay is built on quinn or quiche; document which resolution a node offers.

Tests: frame-end samples under a peer that acknowledges in bursts, a reset
mid-group attributing only the unacknowledged span to `dropped_duration`,
delivery delay under an injected ACK delay staying inside one bucket of the
true RTT, and fallback to group sampling when the hook is unsupported.

## Required

- [Starvation](/quest/m2/qos/starvation.md) - fixes the wire shape and the
  group-granularity fallback
- [poll_acked in web-transport](/quest/m2/quic/ack-hook.md) - the released
  hook this samples through

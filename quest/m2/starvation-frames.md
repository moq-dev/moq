# [S] Starvation at frame granularity

## Goal

Each subscription's acknowledged frontier moves at every frame boundary
instead of once per group, so the starvation histogram and dropped-media
counters from the lag histogram read a frontier at most one frame stale, and a
second histogram reports per-frame delivery delay so viewer jitter is visible
per broadcast. The wire shape from the lag histogram is unchanged apart from
the new histogram.

## Plan

A frame is atomic to the viewer: it is useful only once fully delivered. The
sampler from the lag histogram is unchanged; what changes is how often the
frontier it reads moves. `Writer` in `rs/moq-net/src/coding/writer.rs` gains
a monotonic written-offset counter, since every write funnels through it, so
at each frame end the per-group serve task knows the stream offset and the
frame timestamp. Record `(offset, timestamp, bytes, written_at)` and await
`poll_acked(offset)` on moq-net's own `transport::poll::SendStream`,
interleaved with the writes of later frames so a lagging ACK never stalls
sending. One waiter per stream suffices: offsets are
acknowledged in order for the purpose of this metric, so poll the oldest
pending frame and retire everything at or below the acknowledged offset.

Each acknowledged frame moves the subscription's frontier to that frame's
timestamp, so the interval samples from the lag histogram read a frontier that
is at most one frame stale instead of one group. When a stream
is reset before a frame is acknowledged, `dropped` now counts only the frames
written after the newest acknowledged one, which is the exact media the viewer
lost.

Add a second byte-weighted cumulative histogram of delivery delay:
`acked_at - written_at` per frame, where `acked_at` is when the serve task is
woken by `poll_acked`. Its low edge approaches the path RTT and its spread is
the jitter. Document that the value still includes the return one-way delay
and the peer's delayed-ACK timer, since the hook reports no corrected instant
(decided 2026-10-17 on #5817), and that a backend without the hook leaves the
histogram absent, never zero.

Backends that return unsupported from `poll_acked` keep the group-granularity
sampling from the lag histogram, so the histogram never disappears over qmux or
a browser transport; document which resolution a node offers.

The group-granularity sampler is `Registry::report` in
`rs/moq-net/src/stats.rs`, reading the frontier that `stats::Delivery`
advances on each acknowledged group FIN. Frame-granularity sampling adds the
ACK hook without making either resolution a release gate.

Tests: the frontier tracking frame ends under a peer that acknowledges in
bursts, with interval samples landing one bucket lower than group-granularity
tracking of the same run; a reset mid-group attributing only the
unacknowledged frames to `dropped`; delivery delay staying inside one bucket of the true RTT plus the peer's
`max_ack_delay`; and fallback to group-granularity frontier tracking when the hook is
unsupported.

## Required

- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - the
  hook this samples through

# [S] Buffered exports pick their group order by measurement

## Goal

moq-mux's exports (moq-cli's `ts`, `flv`, `fmp4`, `mp4`, `mkv`, `h264`,
`h265`, HLS, and the RTMP, SRT, and RTSP egress, gst, and ffi consumers built
on its catalog tracks) subscribe in the group order that loses the least
video. The measured case: an MPEG-TS trunk (`moq export ts --delay D`, a
constant 10 to 25 Mb/s) that loses about the length of a 5 s outage even with
an 11.7 s delay, where SRT loses about 0.

## Plan

Decided 2026-11-12 (maintainer, `/quest-import` of
[moq-dev/moq#8559](https://github.com/moq-dev/moq/issues/8559)):
✅ measure Ascending against Descending, then pick (rejected: Ascending for
every moq-mux export without measuring; Ascending only for `ts` and `flv`
under `--delay`; changing `Subscription::default()` in moq-net).

Today every catalog-track subscription in `rs/moq-mux/src/catalog/tracks.rs`
leaves `order` at the default, `Order::Descending`. After an outage the relay
sends the newest groups first, and the group due now expires
(`rs/moq-mux/src/container/consumer.rs`, "current group evicted"). By rough
arithmetic on the issue's path, newest-first leaves the oldest stalled group
waiting about 14.8 s, and oldest-first about 7.4 s. Neither is traced yet.

The maintainer's reasoning, to keep in view:

- Descending exists so recovery is prioritized, assuming an adaptive
  bitrate player that wants the live edge.
- It also keeps a relay that aggregates subscriptions from starving a short
  budget: with `Delay::max` of 1 s and 10 s on one upstream, ascending would
  starve the 1 s subscriber. moq-net already handles this: the upstream
  aggregate is descending when any live subscriber is, and each downstream
  session is served in its own subscriber's order
  (`rs/moq-net/src/model/subscription.rs`). So an export's order changes only
  its own last hop.
- The right order may depend on the maximum latency.

The starting hypothesis to measure mirrors
[Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md):
an export with a nonzero playout delay (`Delay::min`) wants its window from
the start, and goes Ascending; one that chases the live edge stays
Descending.

Measure both orders per export kind on two cases, counting video lost and
the number of skips:

- the issue's outage: one path at 20 Mb/s with `cake`, 100 ms RTT, a
  9.95 Mb/s multiplex, every packet in the relay's direction dropped for 5 s,
  at `--delay` 2 s, 5.75 s, and 11.71 s;
- a sustained deficit on bufferbloat: a rate below the multiplex behind a
  deep but finite queue that overflows.

Ship the order that loses less video for each kind, and record the numbers
here or in the PR. Trace the relay's send order after the return with its
qlog, which confirms or refutes the arithmetic above. The reporter offered to
run their matrix against a branch.

A relay that orders aggregated sends by each subscriber's deadline (earliest
deadline first) instead of "any descending" came up in the interview and is
not planned. The group deadlines in
[Per-stream and per-datagram deadlines](/quest/m4/quic-deadline.md) are the
groundwork it would need.

Public API: none, unless an export grows an order option. Wire: none.

## Required

- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - lite-06 and earlier serve an ascending request newest-first

## Related

- [A path that's back is used within an RTT](/quest/m3/quic-path-return.md) - the transport half of the same outage

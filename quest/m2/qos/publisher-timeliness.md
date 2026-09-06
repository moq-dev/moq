# [M] Publisher timeliness at relay ingest

## Goal

The relay's `moq-stats` ingress rows report, per broadcast, how late media
arrives against the track's own clock and whether frame timestamps stay
monotonic. An operator can tell a publisher that is falling behind, stalling,
or emitting a broken timeline from the relay alone, with no publisher
cooperation.

## Plan

This is the ingress mirror of [starvation](/quest/m2/qos/starvation.md) and
needs no acknowledgments: the relay is the receiver, and a frame is measured
when its last byte arrives, because a partial frame is not useful.

Frame timestamps are relative and jittered with no epoch, so measure each
track against itself. Per track compute each frame's raw offset
`arrival_instant - timestamp` (both converted to one unit) and keep the
minimum of that offset over a sliding window, a minute or so; drift is the
frame's offset minus the windowed minimum. Drift is then "how late this frame
is against the track's own best recent pace". A first-frame anchor lets clock
skew accumulate without bound, and a running minimum bounds only a fast
publisher clock: a clock 100 ppm slow would still read 360 ms late after an
hour. The window bounds both directions to window length times skew, a few
milliseconds, and the re-anchor happens on its own as old minima expire.
Document the window length beside the bucket edges.

Aggregate as a byte-weighted cumulative histogram of drift on the
`Role::Subscriber` (ingress) rows, with the same bucket edges and the same
monotonic contract as the starvation histogram. Beside it keep two cumulative
counters: `timestamp_regressions`, frames whose timestamp is below the
previous group's newest timestamp, and `stalls`, gaps between complete frames
longer than a documented threshold. Compare only across group boundaries for
regressions: B-frame reordering makes presentation timestamps non-monotonic
inside a group, and that is not a broken timeline.

Sample where the relay completes a frame: the moq-lite subscriber's
`run_group` in `rs/moq-net/src/lite/subscriber.rs` (which already decodes the
zigzag timestamp delta) and the IETF subscriber's object path. Pre-lite-05
peers without a timescale stamp frames with `Timestamp::now()`, which would
read as perfectly on time; leave those tracks out of the histogram rather than
report a fiction. Update the stats section of `doc/bin/relay/config.md`.

Tests: a paced publisher lands in the lowest bucket; publishers whose clocks
run 100 ppm slow and 100 ppm fast both stay in the lowest bucket over a
simulated hour; a paused publisher records a
stall and no drift for frames it never sent; a regression across groups is
counted while intra-group reordering is not; a track without a timescale is
excluded.

## Related

- [Starvation](/quest/m2/qos/starvation.md) - the egress half, same
  histogram shape
- [Publisher connection transport](/quest/m2/qos/publisher-transport.md) -
  the publisher's own view of the same uplink

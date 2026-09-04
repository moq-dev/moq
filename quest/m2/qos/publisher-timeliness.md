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
track against itself. Per track keep an anchor `(arrival_instant,
timestamp)` and compute `drift = (arrival - anchor.arrival) - (timestamp -
anchor.timestamp)`. Whenever drift goes negative, the frame arrived earlier
than the anchor predicted, so re-anchor on it and record zero. Drift is then
always "how late this frame is against the track's own best pace", and clock
skew between the publisher's media clock and the relay's wall clock cannot
accumulate the way a first-frame anchor lets it.

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

Tests: a paced publisher lands in the lowest bucket; a publisher whose wall
clock runs slow re-anchors and stays near zero; a paused publisher records a
stall and no drift for frames it never sent; a regression across groups is
counted while intra-group reordering is not; a track without a timescale is
excluded.

## Related

- [Starvation](/quest/m2/qos/starvation.md) - the egress half, same
  histogram shape
- [Publisher connection transport](/quest/m2/qos/publisher-transport.md) -
  the publisher's own view of the same uplink

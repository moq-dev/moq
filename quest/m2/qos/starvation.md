# [M] Starvation: how far behind viewers are, from the relay

## Goal

The relay's `moq-stats` egress rows report, per broadcast, how far behind the
acknowledged frontier of its subscriptions is in media time, and how much
media was abandoned before the peer acknowledged it. A dashboard can read the
relative health of a broadcast's viewers from the relay's point of view
without any per-viewer row on the wire.

## Plan

This is the DELIVERY half of a health verdict and closes the byte-backlog
design in #2733: media-time lag is the primary signal, bytes are the weights.

Define the sample. When a group stream's FIN is acknowledged (which
`Writer::close()` already awaits in `Subscription::serve_group`, in
`rs/moq-net/src/lite/publisher.rs`, and in the IETF publisher), take
`lag = newest frame timestamp the track has produced - newest frame timestamp
in the acknowledged group`. Both are timestamps on the same track, so the
missing epoch does not matter. Lag is zero while the source is paused, and
grows with backlog and with skipped groups until a newer group is acknowledged.
Weight the sample by the group's bytes. This slice samples once per group;
the [frame-granularity quest](/quest/m2/qos/starvation-frames.md) sharpens it
without changing the wire shape, so fix the shape here.

Aggregate as a byte-weighted cumulative histogram on `Traffic`, on the
`Role::Publisher` (egress) rows of the existing `(tier, broadcast, role)`
key: fixed log-spaced buckets of media time, each a monotonic byte counter
(acknowledged bytes whose lag fell in that bucket). Propose buckets at 50 ms,
100 ms, 250 ms, 500 ms, 1 s, 2 s, 5 s, and above, and document the edges in
the stats section of `doc/bin/relay/config.md`. A consumer diffs two snapshots
for a byte-weighted distribution, percentiles, or mean over any interval.
This keeps the crate's cumulative-monotonic contract and the `.z` merge-patch
deltas intact; do not add gauges.

Account for media that is never acknowledged. When a group stream is reset by
skip, expiry, subscriber stop, or session close before its FIN is
acknowledged, add the media duration the peer never got to a cumulative
`dropped_duration`, with `dropped_bytes` and `dropped_groups` beside it. The
duration is the group's newest written timestamp minus its newest acknowledged
timestamp; at group granularity nothing inside the group is known to be
acknowledged, so the whole written span counts, which over-reports until the
frame slice lands. Say so in the docs. Sum tracks per broadcast: the histogram
is in media time, so mixing audio and video is coherent, and byte weighting
already lets video dominate.

Mechanics. `Writer` in `rs/moq-net/src/coding/writer.rs` gains a monotonic
written-offset counter, since every write funnels through it; the per-group
serve task records the newest written timestamp as it goes. The bump points
live on the crate-private `stats::Meter` and `stats::Subscription` carriers
in `rs/moq-net/src/stats.rs`. `moq-stats` sums histograms across nodes in its
aggregate consumer the same way it sums counters today. Update the stats
section of `doc/bin/relay/config.md` with the new fields and their meaning.

Tests: a subscriber that reads at media rate lands in the lowest bucket; a
flow-controlled subscriber walks up the buckets as backlog grows; a skipped
group adds its span to `dropped_duration` and nothing to the histogram; a
session close mid-group counts as dropped; two subscriptions of one broadcast
sum into one row; the aggregate consumer sums two nodes bucket by bucket.

## Closes

- [#2733](https://github.com/moq-dev/moq/issues/2733) - close this issue when
  the quest finishes

## Related

- [Starvation at frame granularity](/quest/m2/qos/starvation-frames.md) -
  samples at every frame end once `poll_acked` is released
- [Publisher timeliness](/quest/m2/qos/publisher-timeliness.md) - the ingress
  mirror on the `Role::Subscriber` rows
- [Viewer feedback](/quest/m2/qos/viewer-feedback.md) - receiver-side
  evidence for the same lag

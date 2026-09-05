# QoS: broadcast health and congestion

## Goal

Publishers, relays, and viewers report the telemetry that broadcast health and
congestion views need: how far behind viewers are according to what the
network has acknowledged, how timely publishers are against their own media
clock, publisher-reported media stats, sender-side transport health, and
sampled viewer feedback. Together they can drive an
unknown/healthy/degraded/unhealthy verdict per broadcast with congestion
visible for viewers in aggregate, the way CMSD does for HLS.

## Plan

Two layers, and the split is what makes this several quests rather than one.
`moq-lite` knows about DELIVERY, what was written toward a peer and what the
peer acknowledged, per subscription, and `hang` knows about MEDIA, which is
what a health verdict has to mean. Neither is useful alone: delivery without
media context cannot tell a slow viewer from a keyframe burst, and media
without delivery cannot tell congestion from a publisher that stopped.

Everything here is distilled per broadcast on the existing `moq-stats` keys.
No per-subscriber or per-session row reaches the wire: many subscriptions
collapse into byte-weighted cumulative histograms, which stay monotonic and
merge-patch friendly, and which any consumer can diff into a distribution.

The counters and channels land here. The moq.pro (downstream) dashboard work,
including the health badge, connection-health drill-down, and stream
preflight, consumes them downstream.

## Quests

- [Starvation](/quest/m2/qos/starvation.md) - per broadcast, how far behind
  the acknowledged frontier of its subscriptions is, in media time, plus the
  media dropped before it was acknowledged
- [Starvation at frame granularity](/quest/m2/qos/starvation-frames.md) - the
  acknowledged frontier moves at every frame boundary through `poll_acked`,
  with a delivery-delay histogram for jitter
- [Publisher timeliness](/quest/m2/qos/publisher-timeliness.md) - per
  broadcast, how late media arrives at the relay against the track's own
  clock, and whether timestamps stay monotonic
- [Publisher media stats](/quest/m2/qos/publisher-stats.md) - a publisher
  reports media cadence and bitrate in its catalog
- [Publisher connection transport](/quest/m2/qos/publisher-transport.md) - a
  publisher reports sender transport health on a relay-bound session channel
- [Viewer feedback](/quest/m2/qos/viewer-feedback.md) - sampled per-viewer QoS
  reaches consumers, so health reflects what viewers experienced

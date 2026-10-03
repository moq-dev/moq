# QoS: broadcast health and congestion

## Goal

Publishers, relays, and viewers report the telemetry that broadcast health and
congestion views need: how far behind viewers are according to what the
network has acknowledged, how timely publishers are against their own media
clock, and what publishers and viewers report for themselves through
[media stats](/quest/m1/stats/README.md). Together they can drive an
unknown/healthy/degraded/unhealthy verdict per broadcast with congestion
visible for viewers in aggregate, the way CMSD does for HLS.

## Plan

Two layers, and the split is what makes this several quests rather than one.
`moq-lite` knows about DELIVERY, what was written toward a peer and what the
peer acknowledged, per subscription, and `hang` knows about MEDIA, which is
what a health verdict has to mean. Neither is useful alone: delivery without
media context cannot tell a slow viewer from a keyframe burst, and media
without delivery cannot tell congestion from a publisher that stopped.

Everything the relay reports is distilled per broadcast on the existing
`moq-stats` keys. No per-subscriber or per-session row reaches the wire from
the relay: many subscriptions collapse into byte-weighted cumulative
histograms, which stay monotonic and merge-patch friendly, and which any
consumer can diff into a distribution. Clients report for themselves through
hang stats and `.echo` feedback tracks, planned in their own line on `main`.

The counters and channels land here. The moq.pro (downstream) dashboard work,
including the health badge, connection-health drill-down, and stream
preflight, consumes them downstream.

Decided (2026-09-28): the line's moq-stats changes break the published
crate. Client stats left the line (2026-09-29)
when media stats moved out of moq-stats onto hang tracks, which are additive
on `main`.

## Required

- [Starvation](/quest/m1/qos/starvation.md) - per broadcast, how far behind
  the acknowledged frontier of its subscriptions is, in media time, plus the
  media dropped before it was acknowledged
- [Final lag sample](/quest/m1/qos/final-lag-sample.md) - a closing
  subscription records its last partial interval instead of losing it
- [Lag dashboard](/quest/m1/qos/lag-dashboard.md) - the demo stats
  dashboard shows viewer lag percentiles and dropped media
- [Publisher timeliness](/quest/m1/qos/publisher-timeliness.md) - per
  broadcast, how late media arrives at the relay against the track's own
  clock, and whether timestamps stay monotonic

## Related

- [Media stats](/quest/m1/stats/README.md) - publishers and viewers report
  their own media, transport, and playback health, the media half of a verdict
- [Loss delay](/quest/m2/cut-through/loss-delay.md) - an ingress counter of
  bytes a loss held back by at least one RTT, on the same rows

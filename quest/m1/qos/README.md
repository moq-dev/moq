# QoS: broadcast health and congestion

## Goal

Publishers, relays, and viewers report the telemetry that broadcast health and
congestion views need: how far behind viewers are according to what the
network has acknowledged, how timely publishers are against their own media
clock, and what publishers and viewers report for themselves in their own
stats broadcasts. Together they can drive an
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
consumer can diff into a distribution. Clients report for themselves, each in
its own `.stats` broadcast on the same layout, so viewer feedback costs
bandwidth only where a publisher or dashboard chose to read it.

The counters and channels land here. The moq.pro (downstream) dashboard work,
including the health badge, connection-health drill-down, and stream
preflight, consumes them downstream.

Decided (2026-09-28): the whole line, including the client stats line, targets
`dev`. The moq-stats schema change
([#4145](https://github.com/moq-dev/moq/pull/4145)) breaks the published
`moq-stats` crate, and a line cannot close with part of it on `main` and part
on `dev`.

## Required

- [Final lag sample](/quest/m1/qos/final-lag-sample.md) - a closing
  subscription records its last partial interval instead of losing it
- [Lag dashboard](/quest/m1/qos/lag-dashboard.md) - the demo stats
  dashboard shows viewer lag percentiles and dropped media
- [Starvation at frame granularity](/quest/m1/qos/starvation-frames.md) - the
  acknowledged frontier moves at every frame boundary through `poll_acked`,
  with a delivery-delay histogram for jitter
- [Publisher timeliness](/quest/m1/qos/publisher-timeliness.md) - per
  broadcast, how late media arrives at the relay against the track's own
  clock, and whether timestamps stay monotonic
- [Client stats](/quest/m1/qos/stats/README.md) - publishers and viewers
  report their own media, transport, and playback health in `.stats`
  broadcasts on the moq-stats layout, and a Rust encoder adapts to its viewers

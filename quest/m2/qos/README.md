# QoS: broadcast health and congestion

## Goal

Publishers, relays, and viewers report the telemetry that broadcast health and
congestion views need: per-subscription backlog, publisher-reported media
stats, sender-side transport health, and sampled viewer feedback. Together
they can drive an unknown/healthy/degraded/unhealthy verdict per broadcast
with congestion visible for viewers in aggregate, a mux-data-style experience.

## Plan

Two layers, and the split is what makes this several quests rather than one.
`moq-lite` knows about BACKLOG, bytes and groups queued versus delivered per
subscription, and `hang` knows about MEDIA, which is what a health verdict has
to mean. Neither is useful alone: backlog without media context cannot tell a
slow viewer from a keyframe burst, and media without backlog cannot tell
congestion from a publisher that stopped.

The counters and channels land here. The moq.pro (downstream) dashboard work,
including the health badge, connection-health drill-down, and stream
preflight, consumes them downstream.

## Quests

- [MoQ backlog counters](/quest/m2/qos/backlog.md) - moq-net maps the
  backend-neutral transport hook into per-subscription backlog and skip
  counters
- [Publisher media stats](/quest/m2/qos/publisher-stats.md) - a publisher
  reports media cadence and bitrate in its catalog
- [Publisher connection transport](/quest/m2/qos/publisher-transport.md) - a
  publisher reports sender transport health on a relay-bound session channel
- [Viewer feedback](/quest/m2/qos/viewer-feedback.md) - sampled per-viewer QoS
  reaches consumers, so health reflects what viewers experienced

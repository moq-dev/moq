# QoS: broadcast health and congestion

## Goal

Publish the relay QoS lag and dropped-counter API after the next release.
The histogram lands on totals and prefix tracks beside the existing maps.
Publisher timeliness, client media health, and dashboard presentation remain
separate additive work in m2.

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

Only the lag counters land here. Client health and preflight are
[client health](/quest/m2/stats/health.md) and
[preflight](/quest/m2/stats/preflight.md) in the media stats line. The
moq.pro (downstream) dashboard work, including the health badge and the
connection-health drill-down, consumes both.

The per-broadcast verdict combining client reports, the relay's starvation,
and publisher timeliness lives downstream. This line only reports the
counters.

Decided (2026-10-06 audit): [stats prefix
tracks](/quest/m2/stats-prefix-tracks.md) lands first, since it is
a prerequisite of the lag API; both are deferred to m2 by the maintainer
on 2026-10-15; the lag histogram lands on the stats totals and
prefix tracks rather than the per-path `publisher.json` and
`subscriber.json` map rows, which stay unchanged for existing readers
(2026-10-12).
Rejected: the histogram first, which would make prefix tracks carry the lag
histogram and dropped counters across.

Decided (2026-10-08): the egress histogram is its own quest,
[viewer lag histogram](/quest/m2/qos/lag-histogram.md), since every other
child reads it. The line's PR, #4133, closed without landing (2026-10-09);
its code is kept on `wip/4133-lag-histogram`.

Decided (2026-09-28): the line's moq-stats changes break the published
crate. Client stats left the line (2026-09-29)
when media stats moved out of moq-stats onto hang tracks, which are additive
on `main`.

## Required

- [Viewer lag histogram](/quest/m2/qos/lag-histogram.md) - the egress `lag`
  histogram and `dropped` counters land on the stats totals and prefix
  tracks

## Related

- [Stats prefix tracks](/quest/m2/stats-prefix-tracks.md) -
  lands first beside the map rows; this line's histograms land on the
  totals and prefix tracks
- [Media stats](/quest/m2/stats/README.md) - publishers and viewers report
  their own media, transport, and playback health, the media half of a verdict
- [Loss delay](/quest/m3/cut-through/loss-delay.md) - an ingress counter of
  bytes a loss held back by at least one RTT, on the same rows

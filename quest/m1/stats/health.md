# [M] Client health from stats and feedback

## Goal

One shared model turns a client's reports into a health sample and a verdict.
Given two snapshots of a publisher's `stats` track or a viewer's `.echo`
feedback, it yields rates and loss as deltas over a stated interval, and
classifies the connection and each rendition as unknown, healthy, degraded,
or unhealthy, naming the observer of every number (the publisher's
self-report, a viewer's report). Missing or stale reports are unknown, never
healthy. Rust and JS compute the same verdict from the same fixtures, so the
demo dashboard, `moq` CLI sinks, [preflight](/quest/m1/stats/preflight.md),
and downstream dashboards share it.

Not here: per-project or per-connection inventories, dashboards, history, or
any relay-side session table. Downstream (moq.pro) keeps its project view.

## Plan

Decided 2026-10-05, from moq.pro's
[connection health](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/qos-connection-health.md):
the health sample and its classification are generic, so they move upstream
into this line; moq.pro keeps the per-project view and dashboard. Rejected:
deferring the model.

The model reads the shapes this line settled after
[#4510](https://github.com/moq-dev/moq/pull/4510): the publisher's `stats`
track named in the catalog, keyed by rendition alias, and viewers' `.echo`
broadcasts. moq.pro's plan predates that and read a `.stats` broadcast that no
longer exists; do not revive it.

Guidance, to be settled while building:

- Inputs are the snapshot types from [the schema](/quest/m1/stats/schema.md):
  `transport` (rtt, rate, loss, sample age) and the per-rendition counters.
  Counters are cumulative, so a sample is the delta of two snapshots over
  their interval; a counter reset (a new epoch) starts over rather than going
  negative.
- Thresholds are a documented default the caller can override, not policy
  baked into the types. Start from a few measured cases rather than guesses.
- A verdict carries its evidence: which inputs drove it and who observed them.
  A browser publisher with only PROBE rtt yields unknown for what it cannot
  see, never a guess.
- The relay's [QoS](/quest/m1/qos/README.md) counters (starvation,
  timeliness) are the other half of a broadcast verdict. Leave a seam to
  combine them, but this quest classifies client reports only.
- Lives beside the snapshot types in `hang` and `@moq/hang` unless building
  it shows a better home.

Prove a degrading publisher self-report, a degrading viewer report, a stale
report going unknown, and a counter reset, from shared fixtures in both
languages. Docs: the media section of `doc/concept/stats.md`.

Public API: new health types in `hang` and `@moq/hang`. Wire: none.

## Required

- [Schema](/quest/m1/stats/schema.md) - the snapshot types the model reads

## Related

- [moq.pro: connection health](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/qos-connection-health.md) - the per-project view built on this model
- [QoS](/quest/m1/qos/README.md) - the relay's delivery counters, the other half of a broadcast verdict

# [M] Client health from stats and feedback

## Goal

One shared model turns a client's reports into a health sample and a verdict.
Given two snapshots of a publisher's `stats` track or a viewer's `.echo`
feedback, it yields rates and loss as deltas over a stated interval, and
classifies the connection and each rendition as unknown, healthy, degraded,
or unhealthy, naming the observer of every number (the publisher's
self-report, a viewer's report). Missing or stale reports are unknown, never
healthy. Rust and JS compute the same verdict from the same fixtures, so
[preflight](/quest/m1/stats/preflight.md) and downstream dashboards share it.
The demo dashboard and a `moq` CLI sink are possible consumers; wiring them
is follow-up work, not this quest.

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
broadcasts. moq.pro's plan predates that and read client stats from a
`.stats` broadcast, which #4510 replaced with these tracks (the relay's own
`.stats/node/<node>` is unchanged); do not revive the client one.

Guidance, to be settled while building:

- Inputs are the snapshot types from [the schema](/quest/m1/stats/schema.md):
  `transport` (rtt, rate, loss, sample age) and the per-rendition counters.
  Rendition rates come from cumulative counters, so a sample is the delta of
  two snapshots over their interval; `transport` gauges (rtt, rate, loss)
  are read as reported, never re-derived, so both languages agree. A reset is detected by a counter decreasing or by the
  reporting broadcast's path or epoch changing
  ([Broadcast epochs](/quest/m0/broadcast-epoch/README.md)); the sample then
  starts over rather than going negative.
- Container sections such as `mpegts` are excluded from the verdict; their
  counters are read by their own consumers
  ([TS health counters](/quest/m2/ts-health-stats.md)).
- Thresholds are a documented default the caller can override, not policy
  baked into the types. Start from a few measured cases rather than guesses.
- A verdict carries its evidence: which inputs drove it and who observed them.
  A browser publisher with only PROBE rtt yields unknown for what it cannot
  see, never a guess.
- This quest classifies client reports only.
- Lives beside the snapshot types in `hang` and `@moq/hang` unless building
  it shows a better home.

Open, for the maintainer (the same question is recorded in
[QoS](/quest/m1/qos/README.md)): whether anything computes a per-broadcast
verdict combining client reports, the relay's starvation, and publisher
timeliness, and where it would live. Nothing here depends on the answer, and
no quest promises it until it is decided.

Prove a degrading publisher self-report, a degrading viewer report, a stale
report going unknown, and a counter reset by both a decrease and an epoch
change, from shared fixtures in both languages. Docs: the media section of `doc/concept/stats.md`.

Public API: new health types in `hang` and `@moq/hang`. Wire: none.

## Required

- [Schema](/quest/m1/stats/schema.md) - the snapshot types the model reads

## Related

- [moq.pro: connection health](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/qos-connection-health.md) - the per-project view built on this model
- [QoS](/quest/m1/qos/README.md) - the relay's delivery counters, which a combined verdict would also read (open above)
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a new epoch is one way a counter reset shows
- [TS health counters](/quest/m2/ts-health-stats.md) - container counters kept out of the verdict

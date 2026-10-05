# [M] Preflight media checks

## Goal

A bounded test run against a published broadcast produces one report a person
can act on: catalog validity, codec support, time to the first sync point
(a group start a decoder can begin at), bitrate and frame cadence per
rendition, sync point interval, audio/video skew,
media timestamp continuity, the [health verdict](/quest/m1/stats/health.md)
from the publisher's stats, and whether the broadcast ends with a clean
unannounce (observed by the check; the publisher, which owns the
`broadcast::Producer`, does the unannounce). A failure names the broken layer
and its evidence instead of one score, and the raw evidence is kept so two
people reach the same diagnosis.

Not here: authorization, route selection, project-scoped test targets, or any
dashboard flow. Downstream (moq.pro) owns those.

## Plan

Decided 2026-10-05, from moq.pro's
[stream preflight](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/qos-preflight.md):
the media checks are generic, so they live upstream in this line; moq.pro
keeps the project-scoped target and dashboard. Rejected: deferring them.

Guidance, to be settled while building:

- A library check over a `broadcast::Consumer` and its catalog, with a thin
  `moq` CLI sink that runs it for a bounded window and prints the report as
  JSON. The verb is the maintainer's call when the PR proposes it.
- Reads only what the broadcast already carries: the catalog, media timestamps
  and group starts, and the `stats` track when the catalog names one. A
  broadcast without a stats track still gets the media checks, with health
  reported as unknown.
- Codec support means "a stock player in this repo can decode it", checked
  against the same rules rendition selection uses, not a separate list.
- One run, one report. No continuous pipeline.
- A publisher still live when the window closes is normal: the run ends at
  the window and reports the unannounce check as not observed, never as a
  failure.

Prove a healthy publisher end to end against a publisher from
[Rust reporters](/quest/m1/stats/rust.md), a publisher outliving the window,
an intra-refresh source (no IDR keyframes) passing the sync point checks, and
deterministic failures for an invalid catalog, an unsupported codec, a
missing first sync point, excessive A/V skew, and a media timestamp jump.
Docs: `doc/bin/inspect.md` is the home, with the sink's flags in
`doc/bin/cli.md`.

Public API: a preflight check and its report type, and a CLI sink. Wire: none.

## Required

- [Client health](/quest/m1/stats/health.md) - the verdict the report includes
- [Rust reporters](/quest/m1/stats/rust.md) - a publisher with a stats track for the end-to-end proof

## Related

- [Publisher timeliness](/quest/m1/qos/publisher-timeliness.md) - the relay's view of the same timestamp lateness and monotonicity
- [Intra-refresh](/quest/m2/intra-refresh/README.md) - a stream without IDR keyframes, which the sync point checks must not fail
- [moq.pro: stream preflight](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/qos-preflight.md) - the project-scoped target and dashboard flow built on these checks

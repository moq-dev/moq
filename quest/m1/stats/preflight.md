# [M] Preflight media checks

## Goal

A bounded test run against a published broadcast produces one report a person
can act on: catalog validity, codec support, time to the first keyframe,
bitrate and frame cadence per rendition, keyframe interval, audio/video skew,
timeline continuity, the [health verdict](/quest/m1/stats/health.md) from the
publisher's stats, and a clean unannounce at the end. A failure names the
broken layer and its evidence instead of one score, and the raw evidence is
kept so two people reach the same diagnosis.

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
  and keyframe flags, and the `stats` track when the catalog names one. A
  broadcast without a stats track still gets the media checks, with health
  reported as unknown.
- Codec support means "a stock player in this repo can decode it", checked
  against the same rules rendition selection uses, not a separate list.
- One run, one report. No continuous pipeline.

Prove a healthy publisher, and deterministic failures for an invalid
catalog, an unsupported codec, a missing first keyframe, excessive A/V skew,
and a timeline jump. Docs: `doc/bin/cli.md` for the sink.

Public API: a preflight check and its report type, and a CLI sink. Wire: none.

## Required

- [Client health](/quest/m1/stats/health.md) - the verdict the report includes

## Related

- [moq.pro: stream preflight](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/qos-preflight.md) - the project-scoped target and dashboard flow built on these checks

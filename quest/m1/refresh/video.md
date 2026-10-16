# [M] Video keyframes on demand

## Goal

Rust and browser video publishers advertise `track::Info::refresh` and
force a keyframe in a new group when `Demand::wanted` passes the largest
group. The encoder's configured max GOP stays as the upper bound. With no
joiners, GOPs run to the max; a joiner gets a fresh keyframe once the GOP's
bytes after its keyframe exceed the ratio.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/refresh/README.md)).

- Open-ended GOPs: a keyframe comes on a refresh or at the max GOP. The cap
  stays as the bound behind hops that cannot forward requests, and against
  decoder drift.
- Scope: encoders that can force a keyframe, which are moq-video's encoder
  backends and `js/publish` through WebCodecs (`keyFrame: true`).
  Passthrough importers (CMAF, TS, RTMP, SRT) cannot force one and do not
  advertise refresh.
- Open: the default ratio for video tracks, and whether it is per rendition.
  Measure the bytes a joiner downloads against the extra keyframes sent on
  a representative stream, then pick a default; the maintainer decides.
- Docs: the encoder options in `doc/` that describe GOP length.

Tests: a forced-keyframe test per backend that supports it, using simulated
time where the encoder allows; a browser test for `js/publish`.

Public API: possibly an encoder config field for the ratio. Wire: none.

## Required

- [Core](/quest/m1/refresh/core.md) - `Demand::wanted` and `track::Info::refresh`

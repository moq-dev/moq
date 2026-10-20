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
- Scope: advertise refresh only on backends whose keyframe forcing is known
  before the track is created, because `track::Info` is immutable. That
  covers `js/publish` through WebCodecs (`keyFrame: true`) and the
  moq-video encoders that support it. V4L2 does not advertise, because some
  drivers reveal that they refuse keyframe requests only after
  `Control::new` has created the track. Passthrough importers (CMAF, TS,
  RTMP, SRT) cannot force one either. Decided in the #5734 review: never
  advertise a request the backend might refuse.
- Remove the 500 ms floor between keyframes from moq-video's `Cuts` and the
  WebCodecs publisher, and let refresh-driven keyframes take over. The byte
  ratio already bounds the rate, and the floor would let a high-bitrate
  joiner download well past the ratio (maintainer, #5734 review).
- Open, settled with the maintainer before any code: where the ratio is
  configured (an encoder option, a rendition option, or a fixed default
  with no public field), its default, and whether it varies per rendition.
  The publishers create their tracks internally, so this decides how
  `track::Info::refresh` gets populated. Measure the bytes a joiner
  downloads against the extra keyframes sent on a representative stream,
  then decide.
- Docs: the encoder options in `doc/` that describe GOP length.

Tests: a forced-keyframe test per backend that supports it, using simulated
time where the encoder allows, and a browser test for `js/publish`. Each
covers a ratio crossing within 500 ms of the previous keyframe.

Public API: set by the open decision above, either an encoder or rendition
config field for the ratio, or none for a fixed default. Wire: none.

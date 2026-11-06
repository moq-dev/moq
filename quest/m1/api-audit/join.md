# [M] Encoders take a join

## Goal

Every publisher-facing knob that spaces a track's join points is a time
called `join`, after the catalog's `media.join` it feeds, in every language
and for both media:

- `@moq/publish`: `Video.Encoder`'s `keyframeInterval` and `Audio.Encoder`'s
  `groupDuration` (and `el.audio.groupDuration`) become `join`, and
  `Rendition.joinInterval` becomes `Rendition.join`.
- `moq-video`: `encode::Config::gop: Gop` becomes `join: Duration`
  (default 2 s), and the `Gop` enum is deleted.
- `moq-audio`: `encode::Options::group_duration` becomes `join`.
- `moq-ffi`: `MoqVideoGop` is deleted, and the video encoder output takes
  `join: Option<Duration>`.
- `moq-c`: the video output's `gop` frame count becomes `join_us` (0 keeps
  the default), and the video and audio hints' `group_duration_us` /
  `has_group_duration` become `join_us` / `has_join`.

`join` means the time between join points at every level of `@moq/publish`:
an encoder's config `join` is the requested value, `Rendition.join` is the
effective value the encoder holds to, and `Broadcast.join` stays the longest
across renditions, published as `media.join.interval`.

Defaults stay the same: video still forces an earlier keyframe for a joiner,
and audio still opens a group on the first frame at least `join` past the
last one. Zero changes: every language refuses a `join` of zero (below). Every rename is a breaking change on `main` with a
MIGRATE note in each package. `release` keeps the old names.

Non-goals: the catalog's own `media.join.interval` field, and
timestamp-driven video joins (the encoder still converts `join` to a frame
count at the configured framerate).

## Plan

Decided 2026-11-06 (maintainer, /quest-complete while queueing
[moq-dev/moq#7279](https://github.com/moq-dev/moq/pull/7279)):

- ✅ **The name is `join`** (`join_us` in C). Audio codecs have no keyframes
  and encoders have no groups, but both knobs set how often a viewer can
  join, which the catalog already calls `media.join`. "join > keyframe."
  The maintainer avoids compound words, and "Interval" reads like a unit.
  Its doc comment must say it is the time between join points, since a bare
  `join` can read like a method or a boolean. Not chosen: `joinInterval`,
  `keyframeInterval` for audio too, `groupInterval`, `interval`, `cadence`
  (collides with capture cadence), `period`.
- ✅ **Scope covers video and the C hints**, not only the audio encoder
  knobs. Not chosen: audio encoder knobs only; dropping the rename.
- ✅ **Rust video takes a plain `Duration`** (shape A). The encoder converts
  it to frames internally, as `Gop::keyframe_every` does today, and
  `Gop::duration` goes with the enum, since the catalog reads `join`
  directly. Not chosen: a non-exhaustive `Join` enum to keep an extension
  point; placing joins by frame timestamp (needs every backend to honor
  forced IDRs and an open codec GOP); a frame count in Rust, FFI, and C.
- ✅ Milestone m1, in this epic, since it makes names mirror across Rust, JS,
  and the bindings.

Decided 2026-11-06 (maintainer, /quest-complete, on Pullfrog's review of
[moq-dev/moq#8036](https://github.com/moq-dev/moq/pull/8036)):

- ✅ **One word at three levels**: `Rendition.joinInterval` becomes
  `Rendition.join` too, so the requested, effective, and broadcast-wide
  values share one name and meaning. Not chosen: keeping `joinInterval`;
  a different name for the encoder knob.
- ✅ **Zero is refused everywhere.** Today JS video keyframes every frame at
  `keyframeInterval: 0`, audio opens a group per frame at zero, and Rust video
  and FFI already refuse it. Each encoder now rejects a zero `join` at
  configuration, which changes behavior for JS and audio callers passing 0;
  each package's MIGRATE note says so. C's zero-initialized video config
  keeps 0 as "the default", since C has no `Option`; the C hints already
  require a nonzero `join_us` when `has_join` is set. Not chosen: zero means
  every frame everywhere; leaving each language's zero as is.

A positive `join` shorter than one frame rounds up to one frame, as
`Gop::keyframe_every` does today.

The catalog advertises the effective `join`, not the requested one, since
`media.join.interval` promises an upper bound and sets the HLS/DASH segment
grid. The video encoder keeps an internal effective duration after rounding
to frames (what `Gop::duration` supplies today): the default 2 s at
30000/1001 fps is 60 frames, or 2002 ms, and a 1 ms `join` at 30 fps is one
frame, about 33.3 ms. That effective value is what `Rendition.join` and the
Rust catalog read. Regression tests cover a fractional frame rate and a
positive sub-frame `join`.

Update `doc/lib/js/publish.md`, `doc/lib/c/index.md`, `doc/lib/rs/moq-audio.md`,
`rs/moq-video/README.md`, and every caller (`moq-cli`, `moq-transcode`, the
backends, the FFI and C wrappers and their language bindings).

Public API: breaking in `@moq/publish`, `moq-video`, `moq-audio`, `moq-ffi`
and its bindings, and the `moq-c` ABI. Wire: none.

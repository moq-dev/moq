# [M] Encoders take a join interval

## Goal

Every publisher-facing knob that spaces a track's join points is a time
called the join interval, named after the catalog's `media.join.interval` it
feeds, in every language and for both media:

- `@moq/publish`: `Video.Encoder`'s `keyframeInterval` and `Audio.Encoder`'s
  `groupDuration` (and `el.audio.groupDuration`) become `joinInterval`.
- `moq-video`: `encode::Config::gop: Gop` becomes
  `join_interval: Duration` (default 2 s), and the `Gop` enum is deleted.
- `moq-audio`: `encode::Options::group_duration` becomes `join_interval`.
- `moq-ffi`: `MoqVideoGop` is deleted, and the video encoder output takes
  `join_interval: Option<Duration>`.
- `moq-c`: the video output's `gop` frame count becomes `join_interval_us`
  (0 keeps the default), and the video and audio hints' `group_duration_us` /
  `has_group_duration` become `join_interval_us` / `has_join_interval`.

Defaults and behavior stay the same: video still forces an earlier keyframe
for a joiner, and audio still opens a group on the first frame at least the
interval past the last one. Every rename is a breaking change on `main` with a
MIGRATE note in each package. `release` keeps the old names.

Non-goals: the catalog's own field, which is already `join.interval`, and
timestamp-driven video joins (the encoder still converts the interval to a
frame count at the configured framerate).

## Plan

Decided 2026-11-06 (maintainer, /quest-complete while queueing
[moq-dev/moq#7279](https://github.com/moq-dev/moq/pull/7279)):

- ✅ **The name is "join interval"** (`joinInterval`, `join_interval`,
  `join_interval_us`). Audio codecs have no keyframes and encoders have no
  groups, but both knobs set how often a viewer can join, which the catalog
  already calls the join interval. "join > keyframe." Not chosen:
  `keyframeInterval` for audio too, `groupInterval`, a bare `interval`.
- ✅ **Scope covers video and the C hints**, not only the audio encoder
  knobs. Not chosen: audio encoder knobs only; dropping the rename.
- ✅ **Rust video takes a plain `Duration`** (shape A). The encoder converts
  it to frames internally, as `Gop::keyframe_every` does today, and
  `Gop::duration` goes with the enum, since the catalog reads the interval
  directly. Not chosen: a non-exhaustive `Join` enum to keep an extension
  point; placing joins by frame timestamp (needs every backend to honor
  forced IDRs and an open codec GOP); a `join_frames` count in Rust, FFI,
  and C.
- ✅ Milestone m1, in this epic, since it makes names mirror across Rust, JS,
  and the bindings.

The video `Encoder::new` refuses a zero interval, as `Gop::validate` refuses
zero frames today; a positive interval shorter than one frame rounds up to one
frame, as `Gop::keyframe_every` does. Audio keeps zero meaning a group per
frame.

Update `doc/lib/js/publish.md`, `doc/lib/c/index.md`, `doc/lib/rs/moq-audio.md`,
`rs/moq-video/README.md`, and every caller (`moq-cli`, `moq-transcode`, the
backends, the FFI and C wrappers and their language bindings).

Public API: breaking in `@moq/publish`, `moq-video`, `moq-audio`, `moq-ffi`
and its bindings, and the `moq-c` ABI. Wire: none.

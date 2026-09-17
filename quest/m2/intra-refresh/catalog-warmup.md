# [S] Catalog warmup on video and audio renditions

## Goal

A video or audio rendition can declare `warmup`, a duration in milliseconds
after a group start during which decoded output is not presentable to a viewer
that did not decode the previous group. Absent means every group start is
presentable at once, which is what every rendition says today. Both catalog
implementations parse and emit it, and the hang draft specifies it. Nothing
honours it yet; the consumer, import, and encoder quests do.

## Plan

- `rs/hang/src/catalog/video/mod.rs` `VideoConfig` and
  `rs/hang/src/catalog/audio/mod.rs` `AudioConfig` gain
  `warmup: Option<Duration>`, serialized as integer milliseconds exactly like
  `jitter` (`DurationMilliSeconds<u64>`), with a one-line doc that says what a
  consumer does with it: decode from the group start, present nothing stamped
  before start plus `warmup` after a non-continuous join, and join that much
  further back. Mirror in `js/hang/src/catalog/video.ts` and `audio.ts` with
  the `u53Schema` optional used for `jitter`, plus round-trip tests beside the
  existing catalog tests in both languages.
- `drafts/draft-lcurley-moq-hang.md`: add a `warmup` field section next to
  `jitter` for both rendition types, with the video (refresh cycle) and audio
  (Opus pre-roll, 80 ms) examples. Relax the group rule at the "Each moq-lite
  group MUST start with a keyframe" text for video only: a video group MUST
  start at a random access point, which is a keyframe unless the rendition
  declares `warmup`, in which case it is a picture from which decoding
  converges within `warmup`. Audio keeps its group rule; there `warmup` is
  pre-roll and says nothing about group starts. Run `just drafts check`.
- Keep the field out of every producer and consumer in this quest; the
  catalog change lands alone so the dependents can proceed in parallel.

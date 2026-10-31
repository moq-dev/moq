# [S] A live keyframe interval change never outruns the declared HLS duration

## Goal

A browser publisher (`@moq/publish`) or a moq-video encoder whose keyframe
interval is raised on a live track, including to infinite, never produces
segments longer than the HLS target duration its catalog declared when the
track was created. The browser publisher's interop cell asserts the
catalog's `hls` field.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7367](https://github.com/moq-dev/moq/pull/7367).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- The declaration is fixed at track creation: JS in [moq-dev/moq#7367](https://github.com/moq-dev/moq/pull/7367)'s `keyframeInterval`
  handling, Rust in `rs/moq-video/src/encode/producer.rs:1073`
  (`with_hls_hint(initial.gop.duration(initial.framerate))`), while the live
  GOP follows later config. A raised interval therefore pushes segments past
  the declared duration.
- Recommended: keep a keyframe at least every declared duration for the
  track's life, by capping the live interval at it. This matches
  [time-addressing consumers](/quest/m1/time-addressing/consumers.md), whose
  `media.join.interval` is fixed for the broadcast instance. Re-declaring
  the duration mid-track is the alternative; ask the maintainer before taking
  it.
- If consumers.md lands first, the `hls` field becomes `media.join.interval`;
  apply the same cap to it and assert that field in the interop cell instead.
- Interop: the browser publisher's cell checks only `@moq/hang` under Node
  and Bun today. Assert the catalog's `hls` field from `@moq/publish` too.

Public API: none (behavior: a raised keyframe interval is capped). Wire: none.

## Required

- [JS HLS field](/quest/m2/js-hls.md) - [moq-dev/moq#7367](https://github.com/moq-dev/moq/pull/7367) publishes the browser's HLS declaration

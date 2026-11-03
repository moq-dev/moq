# [S] A live keyframe interval change never outruns the declared HLS duration

## Goal

A browser publisher (`@moq/publish`) or a moq-video encoder whose keyframe
interval is raised on a live track, including to infinite, never produces
segments longer than the HLS target duration its catalog declared when the
track was created. The browser publisher's interop cell asserts the
catalog's `hls` field. Rust currently has no live GOP setter; its part is
verification of the existing capture declaration and cadence, not a new live
configuration API.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7367](https://github.com/moq-dev/moq/pull/7367).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- Keep the track-creation declaration immutable. The recommended live interval
  cap alone does not establish the Goal; the segment grid also needs a policy
  before implementation (see Waiting).
- Current main still uses `hls`. If [time-addressing consumers](/quest/m1/time-addressing/consumers.md)
  lands first, apply the policy and interop assertion to `media.join.interval` instead. JS reads the hint at
  track creation, while `encoder.ts` reads `config.keyframeInterval` per frame.
  `moq-video` has no live GOP setter: `encode::Encoder` and `encode::Sink` can
  retune bitrate, but GOP is fixed when opened. `publish_capture` opens with the
  same default two-second GOP it probed. Do not add a live GOP API for this quest.
- Reproduced 2026-11-03 with the existing recording VideoEncoder fixture driving
  the real Broadcast, encoder, catalog and HLS recorder. With 100ms frame spacing,
  an initial 2000ms interval declares 4000ms. Change the interval at pts=2500ms,
  after keyframes at 0 and 2000ms:
  - A 3000ms request yields keys at 5000, 8000, 11000 and 14000ms. Segment
    starts are 0, 5000, 8000 and 14000ms: both a 5000ms first segment and a
    6000ms later segment exceed the 4000ms declaration.
  - A prototype `min(requested interval, declared duration)` cap still fails.
    A 6000ms request yields keys at 6000, 10000 and 14000ms; the first segment
    is 6000ms. Infinity supplied as an unchecked config value behaves the same
    under the cap. The checked `Milli(Infinity)` constructor already refuses it.
  - The cause is `js/hang/src/hls.ts`'s Segmenter: a keyframe opens a segment
    only when it enters a later absolute grid cell. A GOP shorter than the
    declaration can still straddle that cell boundary. The Rust segmenter uses
    the same grid rule. Neither the failing test nor the insufficient prototype
    is committed.
  - The grid issue also occurs without a live interval change: with a fixed
    2000ms interval, `encoder.cut()` at 3900ms resets the GOP phase. Keys at
    0, 2000, 3900 and 5900ms yield a 5900ms first segment. Joins and resizes
    also reset that phase. A cap alone leaves this case unchanged.
- After the decision, test actual HLS record durations through live increases,
  decreases, an increase partway through a segment, and on-demand stop/resume.
  Assert `hls` in the actual `@moq/publish` browser interop cell, preserving the
  existing Node/Bun `@moq/hang` record comparisons.
- Do not resolve the separate overrun numbering decision in
  [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652). Grid-aware encoder
  scheduling can use the current segmenter without changing its numbering.

## Waiting

- Maintainer: choose how to reconcile the segment-duration Goal with the
  insufficient cap. Recommended: retain the immutable declaration and extend
  the encoder policy with keyframes at the existing HLS grid boundaries, plus
  an explicit stop/resume policy when no frame arrives by a boundary. Alternative:
  narrow the Goal to bounding GOP duration, where the simple cap suffices but
  HLS segments can still exceed the declaration, even at a fixed interval when
  a join, explicit cut or resize shifts the GOP phase. Re-declaring or replacing the
  track is another alternative requiring an explicit maintainer choice.
  Clarify whether the duration guarantee is exact or permits the first available
  frame after a boundary; even a fixed GOP cannot supply a missing frame.
  Implementation, live-change tests, the browser interop assertion and its local
  review resume after this choice. This does not decide overrun numbering.

Planned public API: none (behavior: bound a raised keyframe interval). Wire: none.
This blocked checkpoint changes only the quest; no encoder policy is implemented.

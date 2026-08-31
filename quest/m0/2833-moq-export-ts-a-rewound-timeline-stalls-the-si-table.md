# [M] moq export ts: a rewound timeline stalls the SI table cadence until the media clock catches up

## Goal

Implement and verify the behavior tracked in [#2833](https://github.com/moq-dev/moq/issues/2833)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

`moq export ts` re-emits the standalone SI tables (SDT on 0x0011, NIT on 0x0010) on a repetition cadence driven by the media timestamp. When the publisher rewinds its timeline, the cadence stops firing until the media clock climbs back past the point it had already reached, which for a large rewind means the SI tables are absent for that entire span.

PAT/PMT do not have this problem, because they are also re-emitted at every video keyframe and a rewind resumes at a keyframe. There is no equivalent trigger for the SI PIDs, and an audio-only program has no keyframe trigger for PAT/PMT either.

#### Mechanism

`due` in `rs/moq-mux/src/container/ts/export.rs` fires when a timestamp reaches a repetition slot later than the last emission's. The stored "last emission" only ever moves forward, so after a backwards jump every subsequent frame compares against a slot the stream has already left behind, and nothing is due until the timeline catches up.

This is not a regression. Before #2825 the same comparison was `elapsed = timestamp - last`, which underflowed on a backwards timestamp and reported not-due, so a rewind stalled the cadence exactly the same way. #2825 changed how the cadence is anchored, not how it behaves across a rewind, and deliberately kept the existing behavior rather than widening scope.

The reason the obvious fix is not simply "fire whenever the slot changes" is that video is emitted in decode order, so a reordered (B-frame) PTS steps backwards constantly. Treating that as a new slot re-emits the tables on every oscillation across a boundary, which was measured on real contribution content at roughly 25x the intended PSI rate (0.071% to 1.747% of the stream). See the discussion on #2825.

#### Direction

A rewind and a reorder are separable by magnitude: reorder depth is bounded by the codec's reorder buffer (the catalog records it as `jitter`, measured at 180 ms on the content above), where a rewind is a discontinuity of arbitrary size. Either a threshold comfortably above the reorder depth, or the existing rewind signal, would do:

`container::Consumer` already detects the rewind that matters here (a newer group whose timestamps jump backwards past the live edge) and exposes it as `discontinuity()`. Reacting to that counter changing, rather than inferring a rewind from timestamp arithmetic, would need no threshold and would reuse a mechanism that already has the group-level context to tell a rewind from a reordered frame.

Worth handling together with whatever the exporter should be doing about PCR and the `discontinuity_indicator` across a rewind, which is a related gap: the emitted PCR jumps backwards with no adaptation-field flag to say so.

## Closes

- [#2833](https://github.com/moq-dev/moq/issues/2833) - close this issue when the quest finishes

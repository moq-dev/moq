# [S] A track's catalog delay is the least it trails the others

## Goal

The catalog estimator's `delay` is the smallest amount a track's frames have
trailed the highest timestamp another track wrote, not the largest. A
constant offset between tracks still shows as `delay`. B-frame reorder, which
only some frames trail by, never does: not on the B-frame track, not on a
sibling rendition, and not on the audio beside it.

## Plan

Reported in [moq-dev/moq#7579](https://github.com/moq-dev/moq/issues/7579)
and verified on `main`. `Order` (`rs/moq-mux/src/catalog/estimate.rs`)
compares presentation timestamps written in decode order, and
`Estimator::order` keeps the maximum trail as `delay`. A B-frame track's high
runs ahead of the frames it writes, so every track written beside it
advertises the reorder as `delay` (40 to 50 ms in the report). The player
then counts it twice, because the measured arrival lateness already holds it:
130 ms of playout against 82 ms on libmoq 0.6.17.

Decided 2026-11-03 (maintainer, `/quest-import`):

- Fix: ✅ `delay` is the minimum trail; subtract each track's declared
  reorder (containers only); something else. The reporter's prototype (decode
  edges, with the reorder taken from the SPS depth through `DeclaredClock`)
  was rejected: "I definitely don't want SPS depth".
- Why the minimum: `delay` exists for a constant offset between tracks, which
  cancels out of the receiver's `measured` (`doc/concept/audio-jitter.md`,
  "Across renditions"). Reference pictures, and the audio of the same decode
  slot, trail by zero, so the minimum drops the reorder. A genuinely late
  track, every frame of which trails, keeps its offset. Variable lateness is
  left to `jitter` and the receiver's measurement, since playout is
  `delay + max(measured, jitter)`.

A track that has never trailed keeps no `delay`. Keep the reporter's lockstep
test (two 50 fps renditions with two B-frames, plus audio, in decode order)
and assert no track gets a `delay`, with and without a declared reorder. Keep
the existing late-track tests. Update `Estimate::delay`'s doc and the `Order`
comment.

Public API: none. Wire: catalog `delay` values shrink for B-frame broadcasts.

## Closes

- [#7579](https://github.com/moq-dev/moq/issues/7579) - catalog `delay` counts B-frame reorder

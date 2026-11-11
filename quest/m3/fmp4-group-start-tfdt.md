# [S] An fMP4 group start never rewinds the decode timeline

## Goal

A GOP-opening fMP4 fragment never starts its decode timeline (`tfdt`) before
the previous fragment's decode timeline ended, for audio and video alike.
Every `moq export fmp4` and HLS output re-imports, even when the source
repeats or nearly repeats a timestamp at a group boundary.

## Plan

Follow-up of [moq-dev/moq#8470](https://github.com/moq-dev/moq/pull/8470),
from its [review thread](https://github.com/moq-dev/moq/pull/8470#discussion_r4535290026).

`encode_fragment` in `rs/moq-mux/src/container/fmp4/export.rs` anchors a run
that opens a GOP (and every audio run) at its first frame's presentation time,
and continues from `decode_end` only for a run cut inside a GOP. Suppose the
previous group's last sample took the catalog cadence because its successor
was on the same or the next output tick: an exact repeat, or, since #8470, a
sub-tick near-repeat. Then that anchor lands at or before the previous
sample's decode time. `fmp4::Import` refuses it with `NonMonotonicDecodeTime`,
and a strict player may too.

This predates #8470: an exact repeated timestamp at a group start already did
it on main. No single PR caused it, since anchoring at the group start's
presentation time is as old as the cadence fallback.

Decided 2026-11-11 (maintainer, follow-up planning in /quest-complete):

- ✅ Plan it as its own quest (rejected: drop it as too rare; fold it into
  [fmp4-declared-clock](/quest/m3/fmp4-declared-clock.md), which only covers
  reordered video with a declared depth).
- ✅ m3 (rejected: m1, m2): a bug fix in shipped code that only sources
  repeating a timestamp at a keyframe hit.
- ✅ The implementer picks the mechanism (rejected: settling it now). The
  candidates:
  - Clamp (recommended): a GOP-opening run starts at the later of its first
    presentation time and the previous fragment's `decode_end`. `encode_at`
    already writes PTS - DTS as a signed composition offset, so a decode time
    after the presentation time is representable, and the next gap
    re-anchors.
  - Shorten the previous sample to end at the group start. This can't fix a
    same-tick repeat, which would need a zero-length sample (#8470 rejected
    inventing one-tick samples), and a live exporter has usually written that
    sample already.

Test: an export whose last frame before a keyframe repeats or nearly repeats
the keyframe's timestamp re-imports through `fmp4::Import`, and its `tfdt`s
increase. Cover video and audio.

Public API: none. Wire: none.

## Related

- [fMP4 decode times from the declared depth](/quest/m3/fmp4-declared-clock.md) - replaces how reordered video's decode times are authored; this quest's rule must hold under it too

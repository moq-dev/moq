# [S] An fMP4 group start never rewinds the decode timeline

## Goal

A GOP-opening fMP4 fragment never starts its decode timeline (`tfdt`) before
the previous fragment's decode timeline ended, for audio and video alike.
Every `moq export fmp4` and HLS output re-imports, even when the source
repeats or nearly repeats a timestamp at a group boundary.

## Plan

Follow-up of [moq-dev/moq#8470](https://github.com/moq-dev/moq/pull/8470),
from its [review thread](https://github.com/moq-dev/moq/pull/8470#discussion_r4535290026).

Three sites re-anchor a GOP-opening run at its first frame's presentation
time instead of continuing the previous decode timeline. The rule applies to
each:

- `encode_fragment` in `rs/moq-mux/src/container/fmp4/export.rs` (`moq export
  fmp4`), which continues from `decode_end` only for a run cut inside a GOP.
- `Fragmenter::emit` in `fragmenter.rs`, on every `push_group`.
- `Muxer::fragment` (`mod.rs` `encode_fragment`), which HLS calls from
  `rs/moq-hls/src/export/rendition.rs`. It is stateless, so the HLS rendition
  must carry the previous segment's decode end into it.

Suppose the previous group's last sample took the catalog cadence because its
successor was on the same or the next output tick. The two cases fail
differently today:

- An exact repeat never reaches the encoder: `emit_fragment` refuses the
  keyframe as `TrackRewound` because its timestamp is at or before
  `track.written`. That guard must accept a keyframe equal to `written` while
  still refusing one strictly before it, a genuine rewind.
- A sub-tick near-repeat (since #8470) exports with a `tfdt` after the
  previous `tfdt` but before the previous fragment's decode end. `fmp4::Import`
  only compares `tfdt`s, so it accepts the overlap, but the two fragments
  decode on top of each other and a strict player may refuse them.

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

Test, for video and audio and on each site above: exports whose last frame
before a keyframe repeats or nearly repeats the keyframe's timestamp succeed,
re-import through `fmp4::Import`, and each fragment's `tfdt` is at least the
previous fragment's `tfdt` plus its sample durations (the check
`export_test.rs` already uses). A keyframe strictly before `written` still
fails with `TrackRewound`.

Public API: none. Wire: none.

## Related

- [fMP4 decode times from the declared depth](/quest/m3/fmp4-declared-clock.md) - replaces how reordered video's decode times are authored; this quest's rule must hold under it too

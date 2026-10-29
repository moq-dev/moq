# [S] A small step back keeps the TS export's video

## Goal

When a source's decode time steps back by more than the export's delay but
within one group, the TS export re-acquires and keeps rendering video,
instead of dropping nearly every video frame until the next group.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7165](https://github.com/moq-dev/moq/pull/7165), which made the
jitter buffer re-acquire on an unmarked step in either direction.

Found while landing #7165: with 600 ms groups and a 500 ms delay, a −560 ms
step drops 105 of 120 video frames over 8 groups, on main's engine as well.
The export's video decode-time reconstruction clamps DTS to stay
non-decreasing, so only the first frame after the step regresses, and it is
still in time. The rest arrive late without a regression, so the jitter
buffer's backward rule never sees the step. Audio is unaffected.

The fix belongs where video decode time is reconstructed, not in the jitter
buffer: the step has to reach the jitter buffer as a regression it can act
on.

[A timebase step restarts the TS import](/quest/m1/ts-reliability/step-restart.md)
turns any rewind from a TS or SRT import into a new epoch, so this remains
for every other source (fMP4, MKV, FLV, RTMP, hang publishers). Kept a
separate quest for that reason.

Tests: an end-to-end TS export of an H.264 + AAC source with the −560 ms
step, which fails on main and keeps video rendering with the fix.

Public API: none. Wire: none.

## Related

- [A timebase step restarts the TS import](/quest/m1/ts-reliability/step-restart.md) - handles a TS or SRT source's rewind at import

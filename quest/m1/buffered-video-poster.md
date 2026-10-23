# [S] Buffered video paints a poster while it fills

## Goal

A `<moq-watch>` with a nonzero `buffer` renders its first decoded video frame
as soon as it decodes, as a poster, and holds video there until playback
starts. Synchronized audio and video share one floor: playback starts once
every enabled track has buffered the floor, and a video-only broadcast waits
on video's own span. Live playback is unchanged.

## Plan

Decided 2026-10-23 by the maintainer, while planning
[Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md):

- ✅ Video holds before playback starts too, but the first frame renders right
  away as a poster while the rest loads.
- ✅ Synchronized A/V shares one floor. Not chosen: video starting on its own
  floor; audio alone deciding the start.
- ✅ Its own quest, ready now: it has no protocol dependency. Not chosen: one
  quest with the ascending work, blocked on lite-07.

Implementation:

- First record what buffered video does at startup today: when the first
  frame paints, and whether it moves before the shared clock starts.
- Reuse the paused poster path (`renderer.out.frame`, read by `Player`)
  rather than adding a second one.
- Gate the shared clock's start on every enabled track reaching the floor,
  consistent with [A/V clock](/quest/m1/av-clock.md), where the audio
  playhead drives `Sync.reference` once audio plays.
- Tests: the poster renders before the floor; video does not advance until
  every enabled track has buffered the floor; video-only starts on its own
  span; live mode is unchanged.

## Related

- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - buffered subscriptions ask for the oldest group first
- [A/V clock](/quest/m1/av-clock.md) - the audio playhead drives `Sync.reference` while audio plays

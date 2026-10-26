# [M] Broadcast sources play in sync

## Goal

A subscribed MoQ broadcast in `moq-studio`, guests included, plays its audio
and video on the publisher's timestamps, so lip sync holds through network
jitter and stalls. Today each track plays at its own live edge, so a stall
can leave audio and video apart.

## Plan

Decided 2026-10-26 (maintainer):

- **Scope:** broadcast sources only. Local devices keep the manual
  per-source offsets in [Audio mixes](/quest/m3/studio/mixes.md).
- **Delay:** reuse `moq play`'s timing, the `--delay auto` target from
  moq-audio's jitter estimate (`rs/moq-audio/src/jitter.rs`,
  `doc/concept/audio-jitter.md`), rather than a second algorithm or a manual
  setting. Show each source's current delay in the UI.

Lift the shared part out of `moq play` into a library both binaries use,
rather than copying it. The video side holds frames until their audio-aligned
time, and a source still repeats its last frame rather than stalling the
canvas. This replaces the live-edge behavior the studio app PR chose, where
replaying a publisher's timestamp gaps pushed audio behind its own video. Here
video waits for audio instead. Update `doc/bin/studio.md`.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the delay estimate this reuses
- [Guests](/quest/m3/studio/guests.md) - guests are broadcast sources and inherit this

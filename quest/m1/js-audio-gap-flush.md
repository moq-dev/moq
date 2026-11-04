# [S] A JS audio capture gap keeps the audio before it

## Goal

In `@moq/publish`, a gap in audio capture (a lost track or a capture
discontinuity, not a disable) publishes every chunk the `AudioEncoder` still
holds for the audio before the gap, then restarts the codec for the audio
after it. This includes a gap inside replayed history, where today most of
the frames one backlog drain submits are lost. Done when every gap goes
through the same flush-then-restart path as a disable, and a test with a gap
in the middle of a replayed backlog publishes every frame on both sides of
it.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7725](https://github.com/moq-dev/moq/pull/7725). That PR made a
disable flush the codec before placing its break marker and restarting it
(`cutAfterFlush` in `js/publish/src/audio/encoder.ts`). It deliberately left
capture gaps on their old path: `submit` calls `encoder.reset()` and
`configure()` as soon as a frame's timestamp isn't contiguous, which throws
away the chunks still in flight. Its comment says those chunks predate the gap,
and they do, so they are audio the publisher had and drops.

- Route a non-contiguous frame through the flush-then-restart path: flush,
  publish what the codec emits, place the gap's marker, restart, then encode
  the frame after the gap. Frames that arrive during the flush must wait
  rather than reach the old codec; the disable path resumes its drain only
  once the flush ends.
- Keep Chrome's output-clock re-base, which is why the restart exists.
- Cover a live gap and a gap inside a replayed backlog, and check that the
  marker sits after every pre-gap chunk.

Public API: none. Wire: none.

## Related

- [Push-to-talk loses no speech](/quest/m1/push-to-talk/README.md) - the same encoder kept alive across a mute

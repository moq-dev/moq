# [M] Buffered audio converges on the shared clock

## Goal

A `<moq-watch>` with a nonzero `buffer` keeps lip sync in steady state: once
a stall, a rise of the `"auto"` delay, or publisher clock drift has left the
audio playhead behind the shared clock, the ring time-stretches back onto it.
It never skips, so a TTS utterance still plays every word. Today buffered
mode turns off the ring's catch-up (`playing = !this.#stalled &&
!this.#buffered` in `js/watch/src/audio/ring-buffer.ts`), so any lag is
permanent. Audio trails video for the rest of the stream, and once the lag
passes `delay + buffer` the container consumer drops whole audio groups
("skipping slow groups"), which is the cut-out reported in
[moq-dev/moq#8031](https://github.com/moq-dev/moq/issues/8031).

Boundaries: video already follows the shared clock and is unchanged. The
clock is not driven by audio, and a slow audio track never holds video. Live
mode (`buffer` of zero) keeps its own catch-up. Native playback has no
buffered mode.

## Plan

Planned 2026-11-06 (maintainer, /quest-import of
[moq-dev/moq#8031](https://github.com/moq-dev/moq/issues/8031)). A
simulation on main with the real `Sync`, container `Consumer`, `Backpressure`
and `AudioRingBuffer` (a copy of `js/watch/src/audio/replay.ts` with buffered
mode on) measured audio behind video: about -12 ms live versus +105 ms
buffered under `"auto"` with jittery then calm phases; 0 versus +300 ms after
400 ms stalls with `delay=100ms buffer=500ms`; and a publisher clock 1% fast
climbing to 1.4 s of lag with 2926 slow-group skips. The reporter's other
claim, that a delay above 500 ms breaks lip sync, did not reproduce (300, 800
and 1500 ms all held about -22 ms).

Decisions:

- Direction: ✅ converge buffered audio by time-stretching / won't fix,
  reply and close / sharpen docs and warn only. Steady-state lip sync comes
  from each track converging on the shared clock, and buffered mode broke
  that even for TTS with video.
- Rename `buffer`: keep / ✅ rename in m2, as
  [Rename buffer to lookahead](/quest/m2/watch-lookahead.md).
- Milestone: ✅ m1, in this epic / m3.

Recommendations:

- Reuse [Watch](/quest/m1/audio-jitter-target/watch.md)'s stretch rather
  than a second mechanism: buffered mode measures the playhead's lag behind
  the shared clock and accelerates until it is back within the floor. It
  never stretches ahead of the clock, since future-dated frames are
  buffered mode's purpose.
- Extend the replay harness with buffered rows (the stall and the drift
  scenarios above), budgeting audio-behind-video and slow-group skips, and
  keep a TTS row asserting no frame is dropped.
- Update the `buffer` docs in `js/watch/src/element.ts`, `js/watch/src/sync.ts`
  and `doc/lib/js/watch.md` to say live streams take their slack from
  `delay`, and that a buffered player catches up after a stall by playing
  slightly faster.

Public API: none expected. Wire: none.

## Required

- [Watch](/quest/m1/audio-jitter-target/watch.md) - lands the ring's time stretch this reuses

## Closes

- [#8031](https://github.com/moq-dev/moq/issues/8031) - buffered playback loses lip sync and cuts audio out on a live stream

## Related

- [Rename buffer to lookahead](/quest/m2/watch-lookahead.md) - the name that invited the misuse
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - the other buffered-mode change to the audio path

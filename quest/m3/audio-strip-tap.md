# [S] Read a bus source's own samples

## Goal

A `moq_audio::bus` caller reads one source's samples as they enter the bus,
before its strip's delay, as `Block`s stamped with their position on the bus
clock, so it can time an event in that source against the clock. A tap that
falls behind skips, as a mix's `Reader` does, and never stalls the bus or
another reader.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan):

- ✅ **A sample tap.** moq-audio exposes the samples and their positions;
  the consumer detects whatever event it is timing.
  Rejected: an onset detector in moq-audio, which would put clap-specific
  DSP and tuning here.
- ✅ **m3**: additive, with a named consumer that is not urgent.

The consumer is the studio's
[clap-to-measure sync delay](https://github.com/moq-dev/studio/blob/main/quest/m0/sync-clap.md):
a clap seen by a camera and heard by a microphone measures how far the
microphone leads, and sets its `Strip::set_delay`. The strip's level meter,
read once a video frame, is too coarse to time that. The tap sits before the
delay because the delay is what is being measured; measuring after it would
include the current setting. The studio bumps its pin in
[its own quest](https://github.com/moq-dev/studio/blob/main/quest/m0/pin-audio-tap.md).

Recommendation, not settled: reuse `Reader` and `Block`, whose `position` is
already the bus clock, rather than a new block type. Taking the samples
before gain and mute too lets a muted microphone be measured. A source with
no tap costs nothing extra. Test with an impulse written at a known point: the
tap reports it at the expected bus position whatever the strip's delay.

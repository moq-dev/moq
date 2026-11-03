# [M] One audio capture feeds the bus and an encoder

## Goal

A moq-audio capture opens its device once and hands its samples to any number
of consumers, such as a program bus channel and an Opus encoder, so
[moq-dev/studio](https://github.com/moq-dev/studio/blob/main/quest/m0/compose/sources.md)'s
microphone and system-audio sources publish their own tracks while they also
feed the program mix. Today the capture stream is crate-private, and the two
public captures, `bus::Capture` and `encode::publish_capture`, each open
their own device, so a source feeds one or the other, never both.

Non-goal: opening a device twice. Two opens can pick different formats and
drift apart, and some devices refuse a second open.

## Plan

Decided 2026-11-02 (maintainer, from moq-dev/studio's source tracks):

- **Shape:** a Producer/Consumer split, mirroring moq-net's, as AGENTS.md
  prefers over callbacks. Opening a capture returns a producer; each consumer
  reads the same samples, layout, gaps, and capture timestamps, and a slow
  consumer cannot stall the device or the others.
- **Breaking:** the split replaces both entry points rather than wrapping
  them, so there is one way to capture. `Bus` takes a capture consumer for a
  channel, and the encode side (`encode::Capture`, `publish_capture`) takes
  one for a track. moq-cli's `publish` updates with it, and the migration is
  written in the changelog. It is an API change, so it lands before the next
  cut.
- **Filters:** the [filter chain](/quest/m3/audio-filters.md) runs on the
  producer side, before any consumer, so the bus and a source's own track
  both carry filtered audio. Gain and mute stay in the bus `Strip`, after the
  split, since a viewer composing the sources mixes levels itself.
- **Failure:** a device that fails or stops ends every consumer with the same
  error, as `Capture::run` and the encode `Status` report it today.

Test: one capture feeds a bus channel and an encoder at once, and both see
every sample with matching timestamps; dropping one consumer leaves the
other running. Benchmark the producer with a growing number of consumers, so
a per-consumer copy or lock shows as a slope rather than a fixed cost.

## Related

- [moq-audio filter chain](/quest/m3/audio-filters.md) - runs before the split
- [Push-to-talk](/quest/m1/push-to-talk/README.md) - an unmute encodes with the mic still open

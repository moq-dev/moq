# [M] Every source of playback latency is reportable, not just measurable in a test

## Goal

A running session can report where its end-to-end audio delay went, stage by
stage, through the public API in both languages. The numbers a user reads when
debugging their own latency are the numbers the harness grades, because they
come from the same place.

## Plan

The audio quality harness lands on ad-hoc debug probes, which is the right
trade to get it running. This quest promotes them.

- Take the stage schema the harness already defines (capture, encode, publish
  flush, network, jitter buffer, decode, render) and expose it the way
  `moq-stats` exposes relay traffic: an observable readout, not a callback.
- Both languages, matching names, per the repo's cross-language rule. Scrutinise
  each exported item: a stage nobody outside can act on stays internal.
- Switch the harness over, deleting the probes it replaces. A ledger with no
  consumer is how this drifts from reality.
- The stages must sum to the measured end-to-end delay within a stated
  tolerance, and that identity is itself a test. It only holds on the harness's
  terms: one duration unit, every timestamp on a named clock, and stages as
  exclusive spans that cannot both claim the same milliseconds. Inherit those
  from the schema rather than restating them. An unaccounted remainder is the
  bug this whole line exists to find, so it gets a name and a number rather
  than being absorbed into a neighbouring stage.

## Required

- [Audio quality harness](/quest/m2/audio-quality-harness/README.md) - defines the stage schema and lands the probes this promotes

## Related

- [Audio quality harness](/quest/m2/audio-quality-harness/README.md) - defines the stages and is the first consumer
- [QoS](/quest/m2/qos/README.md) - relay-side health, the same idea from the other end

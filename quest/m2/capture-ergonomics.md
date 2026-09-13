# [S] Plan capture ergonomics

## Goal

Three papercuts in the capture surface, none blocking but each visible the
first time someone hits it.

This is a planning dispatch. Verify each current limitation against source,
then replace this quest with independently completable crop, audio mixing,
and format-validation quests. Each needs a chosen public API, ownership
boundary, supported/refused cases, and CI acceptance tests. Ask the maintainer
about unsettled crop coordinates and audio clock/mixing policy before coding.
Identify any published API break for M1; do not hold independent validation
work behind crop or mixing design.

## Plan

- **Region and crop capture.** No knob exists anywhere; `capture::Config`
  carries source, device, width, height, and framerate. Cropping a region of a
  display is the common screen-share case that currently requires capturing
  the whole thing.
- **Mixing multiple audio devices.** One device, one track. A screen share
  wanting microphone plus system audio has no way to say so, which is exactly
  the combination the `System` source makes newly reachable.
- **Format overrides are unvalidated.** A requested sample rate is applied
  without checking the device's supported ranges, so a bad combination fails
  inside cpal's `build_input_stream` instead of erroring with something that
  names the problem. The supported ranges are already enumerable, which is
  what `moq devices` reads.

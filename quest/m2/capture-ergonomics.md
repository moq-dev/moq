# [S] Plan capture ergonomics

## Goal

Two papercuts in the capture surface, none blocking but each visible the
first time someone hits it.

This is a planning dispatch. Verify each current limitation against source,
then replace this quest with independently completable crop and audio mixing
quests. Each needs a chosen public API, ownership
boundary, supported/refused cases, and CI acceptance tests. Ask the maintainer
about unsettled crop coordinates and audio clock/mixing policy before coding.
Keep additions compatible with the settled main capture contracts. Identify any
published API break for a separate maintainer decision; independent format
validation already has its own quest.

## Plan

- **Region and crop capture.** No knob exists anywhere; `capture::Config`
  carries source, device, width, height, and framerate. Cropping a region of a
  display is the common screen-share case that currently requires capturing
  the whole thing.
- **Mixing multiple audio devices.** One device, one track. A screen share
  wanting microphone plus system audio has no way to say so, which is exactly
  the combination the `System` source makes newly reachable.
  Preserve exclusive AEC microphone ownership and define clock alignment
  before sharing processed microphone input.

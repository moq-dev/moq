# [M] hang specifies a layer layout track

## Goal

`drafts/draft-lcurley-moq-hang.md`, `rs/hang`, and `js/hang` define a layer
layout track: a versioned JSON track in snapshot mode on the media clock that
names each layer's track and gives its transform, crop, z-order, opacity,
visibility, keyframes, priority, and whether a viewer may toggle it. An audio
layer also carries its gain, mute, pan, and delay.

## Plan

Start from the format moq-dev/studio's watch prototype settled, and keep only
what every player can reproduce. A consumer refuses an unknown version loudly
and plays the main picture alone. Layers name tracks and catalog references,
never files.

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit): an audio layer
carries pan and delay as well as gain and mute. The studio's strip has all
four (`rs/moq-scene/src/collection.rs` in moq-dev/studio; pan shipped in
[moq-dev/studio#116](https://github.com/moq-dev/studio/pull/116)), and a
source's own track is taken before the strip applies them. Without the delay
in the layout, a microphone the creator delayed for sync and published on its
own track would play out of sync for viewers.

## Waiting

- moq-dev/studio: its `moq-studio --watch` prototype plays program plus
  layers from the provisional `dev.moq.studio.layers` track, planned as
  `quest/m2/compose/watch.md` there. Check: that quest's file is gone from
  moq-dev/studio's `main` because its PR merged.

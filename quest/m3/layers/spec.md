# [M] hang specifies a layer layout track

## Goal

`drafts/draft-lcurley-moq-hang.md`, `rs/hang`, and `js/hang` define a layer
layout track: a versioned JSON track in snapshot mode on the media clock that
names each layer's track and gives its transform, crop, z-order, opacity,
visibility, keyframes, gain and mute, priority, and whether a viewer may
toggle it.

## Plan

Start from the format moq-dev/studio's watch prototype settled, and keep only
what every player can reproduce. A consumer refuses an unknown version loudly
and plays the main picture alone. Layers name tracks and catalog references,
never files.

## Waiting

- moq-dev/studio: its `moq-studio --watch` prototype plays program plus
  layers from the provisional `dev.moq.studio.layers` track, planned as
  `quest/m2/compose/watch.md` there. Check: that quest's file is gone from
  moq-dev/studio's `main` because its PR merged.

# [M] moq play composites layers

## Goal

`moq play` plays a broadcast that carries a layer layout track: it composites
each visible layer over the main picture where and when the layout says, and
mixes each audio layer in at its gain and mute. A broadcast without one plays
as today.

## Plan

Fetch only visible layers, at the layout's priorities through moq-lite's
subscription priorities. Prove placement and timing against the epic's
fixture. moq-dev/studio's `hang-layers` quest deletes its watch prototype
once this lands.

## Required

- [hang specifies a layer layout track](/quest/m3/layers/spec.md) - what `moq play` composites

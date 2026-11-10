# [M] moq play composites layers

## Goal

`moq play` plays a broadcast that carries a layer layout track: it composites
each visible layer over the main picture where and when the layout says,
mixes each audio layer in at its gain, mute, pan, and delay, and lets a
viewer toggle each layer the layout marks toggleable. A broadcast without one
plays as today.

## Plan

Fetch only visible layers, at the layout's priorities through moq-lite's
subscription priorities, and unsubscribe a toggled-off layer. Prove placement
and timing against the epic's fixture.

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit): the viewer
toggle and the priority mapping are this quest's work, not only
`@moq/watch`'s. moq-dev/studio decided on 2026-11-08 (its
`quest/m2/compose/controls.md`) that viewer controls land in the native
player, and its `hang-layers` quest deletes its prototype player once this
lands, so this quest is their home. Subscriptions are per track, not per
layer, as that quest settled: a track several layers show stays subscribed
while any of them is on, at the highest priority among them.

## Required

- [hang specifies a layer layout track](/quest/m3/layers/spec.md) - what `moq play` composites

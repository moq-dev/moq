# [M] moq play plays compositions

## Goal

`moq play` plays a composition: it opens each child broadcast the
composition lists, draws each visible picture where and when the
composition says, on the composition's clock through the child's offset,
mixes each child's sound in at its gain, mute, pan, and delay, and lets a
viewer toggle each child the composition marks toggleable. A broadcast
without a composition plays as today.

## Plan

Fetch only children a visible picture or an unmuted sound uses, at the composition's priorities through moq-lite's
subscription priorities, and unsubscribe a toggled-off child. Prove placement
and timing against the epic's fixture.

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit): the viewer
toggle and the priority mapping are this quest's work, not only
`@moq/watch`'s. moq-dev/studio decided on 2026-11-08 (its
`quest/m2/compose/controls.md`) that viewer controls land in the native
player, and its `hang-layers` quest deletes its prototype player once this
lands, so this quest is their home. Subscriptions are per track, not per
child, as that quest settled: a track several children show stays subscribed
while any of them is on, at the highest priority among them.

## Required

- [hang specifies a composition](/quest/m3/layers/spec.md) - what `moq play` plays

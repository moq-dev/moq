# [M] The watch player plays compositions

## Goal

`@moq/watch` plays a composition: it opens each child broadcast the
composition lists, places each visible picture child where and when the
composition says, on the composition's clock through the child's offset,
mixes each sound child in at its gain, mute, pan, and delay, and lets a
viewer toggle each child the composition marks toggleable. A broadcast
without a composition plays as today.

## Plan

Fetch only visible children, at the composition's priorities through moq-lite's
subscription priorities, and unsubscribe a toggled-off child. Prove placement
and timing against the epic's fixture, matching `moq play` within its
tolerance.

## Required

- [hang specifies a composition](/quest/m3/layers/spec.md) - what `@moq/watch` plays

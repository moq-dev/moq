# [M] The watch player composites layers

## Goal

`@moq/watch` plays a broadcast that carries a layer layout track: it
composites each visible layer over the main picture where and when the layout
says, mixes each audio layer in at its gain, mute, pan, and delay, and lets a
viewer toggle each layer the layout marks toggleable. A broadcast without one
plays as today.

## Plan

Fetch only visible layers, at the layout's priorities through moq-lite's
subscription priorities, and unsubscribe a toggled-off layer. Prove placement
and timing against the epic's fixture, matching `moq play` within its
tolerance.

## Required

- [hang specifies a layer layout track](/quest/m3/layers/spec.md) - what `@moq/watch` composites

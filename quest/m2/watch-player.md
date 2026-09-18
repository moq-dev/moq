# [M] A headless Watch.Player assembles the pipeline once

## Goal

`@moq/watch` exports a `Player` that owns the eight-object pipeline
(`Broadcast`, `Sync`, `Video.Source/Decoder/Renderer`, `Audio.Source/
Decoder/Emitter`, `Text.*`) and the enable wiring between them, so
`<moq-watch>` is attributes over a `Player`, room's `Member` is a `Player`
plus a canvas, and an application that wants no element writes neither.
Today `js/watch/src/element.ts`, `js/room/src/remote.ts`, and moq.pro's
`VoiceTestClient.svelte` each rebuild the same graph and each must remember
that `Emitter.out.enabled` feeds `Decoder.in.enabled`.

## Plan

`new Watch.Player({ origin, probe, name, announced, catalogFormat,
...controls })` exposing `broadcast/video/audio/text/renderer/emitter/sync`
as readonly fields. `Broadcast`, `Sync`, and the per-kind classes stay
exported for composition but stop being the front door.

Public API: additive on @moq/watch and @moq/room. Wire: none.

## Required

- [Watch and publish shapes](/quest/m1/api-watch-publish.md) - the pieces settle before the assembly is named
- [Merge dev](/quest/m1/merge-dev.md) - starts on main

## Related

- [A/V clock](/quest/m2/plan-av-clock.md) - per-track sync handles the Player would own

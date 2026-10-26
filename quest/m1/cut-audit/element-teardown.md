# [S] Removing a media element releases its devices

## Goal

Removing `<moq-publish>` stops its camera, microphone, screen capture,
AudioContext, and preview, so the browser's recording indicator turns off.
Removing `<moq-watch>` releases its decoded frames and WebGPU device. A
screen share picked after its run was torn down is stopped. No element is
pinned in memory by its own cleanup registry.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Privacy for publish.

- **Publish keeps capturing** (source-inferred, inherited):
  `disconnectedCallback` (`js/publish/src/element.ts:333`) only flips
  `#enabled`, which gates the connection and broadcast; sources are gated on
  `#videoEnabled`/`#audioEnabled` (`:197`, `:379`), so camera, mic, capture,
  the audio worklet, and the preview keep running after an SPA unmounts the
  element.
- **Watch keeps GPU state** (source-inferred; the GPUDevice part is new on
  main, `js/video` is absent on release): the last decoded `VideoFrame`
  (`js/watch/src/video/decoder.ts:212`), the renderer's clone, and the
  `GPUDevice` (`js/video/src/renderer.ts:150`) survive disconnect.
- **The registry backstop never fires** (reproduced under bun, 1000 of 1000
  targets never finalized): `cleanup.register(this, this.signals)`
  (`publish/src/element.ts:188`, `watch/src/element.ts:229`) holds the
  Effect strongly, and its closures capture `this`. Delete both registries.
- **Late screen share** (source-inferred, inherited):
  `js/publish/src/source/screen.ts:87` never stops a stream the user picks
  after `effect.race` returned undefined (a source switch or close while the
  picker is open, realistic in Firefox). Stop the pending promise's tracks
  in a cleanup, as `camera.ts:135` does.

Gate source, capture, preview, and renderer surface on `#enabled`, and clear
the decoder frame when the broadcast is disabled. Writes coalesce per
microtask, so a DOM move costs nothing. The UI elements
(`watch/src/ui/element.ts:59`) already follow the `js/AGENTS.md`
connected/disconnected rule.

Public API: none (behavior: capture stops on disconnect). Wire: none.

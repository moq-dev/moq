# [M] The watch element renders spatial video through WebXR layers

## Goal

On a headset browser with WebXR layers, `<moq-watch>` presents a rendition
whose catalog says `projection: equirectangular` in an immersive session
through an `XREquirectLayer`: the horizontal span comes from `fov`, and the
layer layout from `stereo`, mapping `left-right` to `stereo-left-right`,
`top-bottom` to `stereo-top-bottom`, and `mono` or absent to `mono`. A flat
rendition, and a browser without WebXR layers, play as today.

## Plan

Decided with the maintainer on 2026-10-20, from the follow-ups of
[#5543](https://github.com/moq-dev/moq/pull/5543), which landed the fields.

- `XRMediaBinding` layers take an `HTMLVideoElement`, which `<moq-watch>`
  does not use (it decodes with WebCodecs), so an `XRWebGLBinding` layer fed
  each decoded frame is the likely path. Check whether a WebGPU XR binding is
  usable yet.
- Entering an immersive session needs a user gesture, so the UI offers it
  only when the browser supports layers and the rendition is spatial.
- Ambisonic audio playback is out of scope; propose it as a follow-up.

Tests: a mocked XR session checks the layer each catalog produces, all three
layouts included, and is wired
into CI; a manual run on a headset browser proves playback.

Public API: possibly a `<moq-watch>` attribute to enter or disable immersive
playback. Wire: none.

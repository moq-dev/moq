---
title: "@moq/video"
description: Draw video frames into a canvas through WebGPU or Canvas2D
---

# @moq/video

[![npm](https://img.shields.io/npm/v/@moq/video)](https://www.npmjs.com/package/@moq/video)

The renderer behind [`<moq-watch>`](/lib/js/watch) and the
[`<moq-publish>`](/lib/js/publish) canvas preview. It draws `VideoFrame`s into
a `<canvas>` or `OffscreenCanvas`, rotated and mirrored as the catalog says,
sized to the video's display size.

```ts
import * as Video from "@moq/video";

const renderer = new Video.Renderer({ canvas, frame, presentation: { rotation: 90 } });
```

## WebGPU or Canvas2D

The `backend` input (also an attribute on the elements) picks the graphics API:

- `auto` (default): WebGPU where it can render a `VideoFrame`, else Canvas2D.
  WebGPU imports each frame without a copy where the browser allows, in the
  frame's own colour space (sRGB or Display P3), and draws faster, most of all
  on Firefox and Safari. WebGPU is still missing on Firefox for Linux, Intel
  Mac, and Android, and on Linux Chrome with many GPUs, so the console logs
  which one was picked and why.
- `webgpu`: WebGPU only. Where it is missing, nothing draws and `out.error` is
  `"unsupported"`.
- `2d`: Canvas2D only.

The choice is made once per canvas, before drawing, since a canvas keeps the
first kind of context it hands out. A lost GPU device is replaced. If no GPU
comes back, drawing stops and `out.error` is `"surface-lost"`: hand the
renderer a fresh canvas, and `auto` picks again. The elements swap their
`<canvas>` child for a copy themselves.

The Rust twin is [`moq-video`](/lib/rs/moq-video), whose `render` module draws
on the GPU with wgpu.

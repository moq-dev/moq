<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/video

[![npm version](https://img.shields.io/npm/v/@moq/video)](https://www.npmjs.com/package/@moq/video)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Draw [Media over QUIC](https://moq.dev) video frames into a canvas, through WebGPU where the
browser can render a `VideoFrame` with it and Canvas2D everywhere else. `@moq/watch` and the
`@moq/publish` preview render through it.

```bash
npm add @moq/video
```

```ts
import { Signal } from "@moq/signals";
import * as Video from "@moq/video";

const frame = new Signal<VideoFrame | undefined>(undefined);
const renderer = new Video.Renderer({
	canvas: document.querySelector("canvas") ?? undefined,
	frame,
	display: { width: 1280, height: 720 },
	presentation: { rotation: 0, flip: false },
	backend: "auto", // or "webgpu" or "2d"
});

// Each new frame paints on the next animation frame. The renderer clones what it keeps.
frame.set(decoded);
```

`backend: "auto"` probes WebGPU once per canvas, before touching it, and logs which API it
picked. A lost GPU device is replaced; with no replacement, drawing stops and `out.error` is
`"surface-lost"` until the `canvas` input gets a fresh canvas. An explicit `"webgpu"` where it is
missing reports `"unsupported"`.

The Rust twin is [`moq-video`](https://crates.io/crates/moq-video).

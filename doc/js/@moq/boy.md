---
title: "@moq/boy"
description: Browser player for the MoQ Boy demo
---

# @moq/boy

[![npm](https://img.shields.io/npm/v/@moq/boy)](https://www.npmjs.com/package/@moq/boy)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Browser-side player for the [MoQ Boy demo](https://moq.dev/boy). Discovers active game sessions over MoQ, renders their video/audio, and publishes button input back to the emulator.

The server-side emulator lives in the [moq-boy](/rs/crate/moq-boy) Rust crate.

## Installation

```bash
bun add @moq/boy
# or
npm add @moq/boy
```

## Web Component

```html
<script type="module">
	import "@moq/boy/element";
</script>

<moq-boy
	url="https://relay.example.com"
	prefix-game="boy"
	prefix-viewer="viewer/boy">
</moq-boy>
```

**Attributes:**

- `url` (required) — Relay server URL
- `prefix` — Base path prefix (default: `boy`). Derives `prefix-game` and `prefix-viewer`.
- `prefix-game` — Path prefix for game broadcasts (default: `{prefix}/game`)
- `prefix-viewer` — Path prefix for viewer broadcasts (default: `{prefix}/viewer`)

The element manages its own UI (game grid, per-game canvas, on-screen buttons, stats) inside a Shadow DOM.

## How it works

- **Discover games** — subscribes to announcements under `prefix-game` and mounts each suffix as a `Game`.
- **Subscribe on demand** — video and audio tracks are only subscribed while the game is visible in the viewport, so the server-side emulator auto-pauses when nothing is being watched.
- **Publish input** — button presses go out on a JSON track under `prefix-viewer/{name}/{viewerId}/command`.
- **Live status** — the server publishes a `status` track with currently pressed buttons and per-viewer latency, used to highlight shared input.

See [demo/moq-boy](/demo/moq-boy) for the full architecture and track layout.

## Related Packages

- **[@moq/watch](/js/@moq/watch)** — Used under the hood for video/audio playback
- **[@moq/lite](/js/@moq/lite)** — Core pub/sub transport
- **[moq-boy](/rs/crate/moq-boy)** — The Rust emulator/publisher

## Source

[js/moq-boy](https://github.com/moq-dev/moq/tree/main/js/moq-boy)

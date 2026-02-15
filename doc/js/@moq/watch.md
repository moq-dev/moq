---
title: "@moq/watch"
description: Subscribe to and render MoQ broadcasts
---

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Subscribe to and render MoQ broadcasts. Provides both a JavaScript API and a `<moq-watch>` Web Component, plus an optional `<moq-watch-ui>` SolidJS overlay.

## Installation

```bash
bun add @moq/watch
# or
npm add @moq/watch
```

## Web Component

```html
<script type="module">
    import "@moq/watch/element";
</script>

<moq-watch
    url="https://relay.example.com/anon"
    path="room/alice"
    controls>
    <canvas></canvas>
</moq-watch>
```

**Attributes:**
- `url` (required) — Relay server URL
- `path` (required) — Broadcast path/name
- `controls` — Show playback controls (boolean)
- `paused` — Pause playback (boolean)
- `muted` — Mute audio (boolean)
- `volume` — Audio volume (0–1, default: 1)

## UI Overlay

Import `@moq/watch/ui` for a SolidJS-powered overlay with buffering indicator, stats panel, and playback controls:

```html
<script type="module">
    import "@moq/watch/element";
    import "@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch
        url="https://relay.example.com/anon"
        path="room/alice">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

The `<moq-watch-ui>` element automatically discovers the nested `<moq-watch>` and wires up reactive controls.

## JavaScript API

```typescript
import * as Watch from "@moq/watch";

const broadcast = new Watch.Broadcast(connection, {
    enabled: true,
    name: "alice",
    video: { enabled: true },
    audio: { enabled: true },
});

// Reactive controls
broadcast.volume.set(0.8);
broadcast.paused.set(false);
```

## Related Packages

- **[@moq/publish](/js/@moq/publish)** — Publish media to MoQ broadcasts
- **[@moq/hang](/js/@moq/hang/)** — Core media library (catalog, container, support)
- **[@moq/ui-core](/js/@moq/ui-core)** — Shared UI primitives
- **[@moq/lite](/js/@moq/lite)** — Core pub/sub transport protocol

---
title: "@moq/hang"
description: Media library with Web Components
---

# @moq/hang

[![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

High-level media library for real-time streaming using [Media over QUIC](https://moq.dev), built on top of [@moq/lite](/typescript/lite).

## Overview

`@moq/hang` provides:

- **Web Components** - Easiest way to add MoQ to your page
- **JavaScript API** - Advanced control for custom applications
- **WebCodecs integration** - Hardware-accelerated encoding/decoding
- **Reactive state** - Built on `@moq/signals`

## Installation

```bash
npm add @moq/hang
# or
pnpm add @moq/hang
bun add @moq/hang
```

## Web Components

The fastest way to add MoQ to your web page. See [Web Components](/typescript/web-components) for details.

### Publishing

```html
<script type="module">
    import "@moq/hang/publish/element";
</script>

<hang-publish
    url="https://relay.example.com/anon"
    path="room/alice"
    audio video controls>
    <!-- Optional: preview -->
    <video muted autoplay></video>
</hang-publish>
```

### Watching

```html
<script type="module">
    import "@moq/hang/watch/element";
</script>

<hang-watch
    url="https://relay.example.com/anon"
    path="room/alice"
    controls>
    <!-- Optional: canvas for video -->
    <canvas></canvas>
</hang-watch>
```

### Video Conferencing

```html
<script type="module">
    import "@moq/hang/meet/element";
</script>

<hang-meet
    url="https://relay.example.com/anon"
    path="room123"
    audio video controls>
</hang-meet>
```

[Learn more about Web Components →](/typescript/web-components)

## JavaScript API

For advanced use cases:

```typescript
import * as Hang from "@moq/hang";

// Create connection
const connection = new Hang.Connection("https://relay.example.com/anon");

// Publishing media
const publish = new Hang.Publish.Broadcast(connection, {
    enabled: true,
    name: "alice",
    video: { enabled: true, device: "camera" },
    audio: { enabled: true },
});

// Subscribing to media
const watch = new Hang.Watch.Broadcast(connection, {
    enabled: true,
    name: "alice",
    video: { enabled: true },
    audio: { enabled: true },
});

// Everything is reactive
publish.name.set("bob");
watch.volume.set(0.8);
```

## Features

### Real-time Latency

Uses WebTransport and WebCodecs for sub-second latency:

```typescript
const watch = new Hang.Watch.Broadcast(connection, {
    name: "live-stream",
    // Latency optimizations
    video: { enabled: true },
});
```

### Device Selection

Choose camera or screen:

```typescript
const publish = new Hang.Publish.Broadcast(connection, {
    name: "my-stream",
    video: {
        enabled: true,
        device: "camera", // or "screen"
    },
});

// Switch devices
publish.video.device.set("screen");
```

### Quality Control

Control encoding quality:

```typescript
const publish = new Hang.Publish.Broadcast(connection, {
    name: "my-stream",
    video: {
        enabled: true,
        bitrate: 2_500_000, // 2.5 Mbps
        framerate: 30,
    },
});
```

### Playback Controls

```typescript
const watch = new Hang.Watch.Broadcast(connection, {
    name: "stream",
});

// Pause/resume
watch.paused.set(true);
watch.paused.set(false);

// Volume
watch.muted.set(false);
watch.volume.set(0.8);
```

## Reactive State

Everything uses signals from `@moq/signals`:

```typescript
import { react } from "@moq/signals/react";

const publish = document.querySelector("hang-publish") as HangPublish;

// Convert to React signal
const videoSource = react(publish.video.media);

useEffect(() => {
    previewVideo.srcObject = videoSource();
}, [videoSource]);
```

## Supported Codecs

**Video:**
- H.264 (AVC) - Best compatibility
- H.265 (HEVC) - Better compression
- VP8 / VP9 - Open codec
- AV1 - Latest, best compression

**Audio:**
- Opus - Best for voice/music
- AAC - Good compatibility

Codec selection is automatic based on browser support.

## Browser Support

Requires:
- **WebTransport** - Chrome 97+, Edge 97+
- **WebCodecs** - Same browsers
- **WebAudio** - All modern browsers

## Examples

Check out [hang-demo](https://github.com/moq-dev/moq/tree/main/js/hang-demo) for:

- Video conferencing
- Screen sharing
- Chat integration
- Quality selection UI

[View more examples →](/typescript/examples)

## Framework Integration

Works with any framework:

- **React** - Via `@moq/signals/react`
- **SolidJS** - Via `@moq/signals/solid` or `@moq/hang-ui`
- **Vue** - Via `@moq/signals/vue`
- **Vanilla JS** - Direct Web Components

## Protocol Specification

See the [hang specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-hang.html).

## Next Steps

- Learn about [Web Components](/typescript/web-components)
- Use [@moq/lite](/typescript/lite) for custom protocols
- View [code examples](/typescript/examples)
- Deploy with the [Deployment guide](/guide/deployment)

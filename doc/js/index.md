---
title: TypeScript Libraries
description: TypeScript/JavaScript implementation for browsers
---

# TypeScript Libraries

The TypeScript implementation brings MoQ to web browsers using modern APIs like WebTransport and WebCodecs.

## Core Libraries

### @moq/lite

[![npm](https://img.shields.io/npm/v/@moq/lite)](https://www.npmjs.com/package/@moq/lite)

Core pub/sub transport protocol for browsers. Implements the [moq-lite specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html).

**Features:**
- WebTransport-based QUIC
- Broadcasts, tracks, groups, frames
- Browser and server-side support (with polyfill)

[Learn more](/js/@moq/lite)

### @moq/hang

[![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)

High-level media library with Web Components for streaming audio and video.

**Features:**
- Web Components (easiest integration)
- JavaScript API for advanced use
- WebCodecs-based encoding/decoding
- Reactive state management

[Learn more](/js/@moq/hang/)

## UI Components

### @moq/hang-ui

[![npm](https://img.shields.io/npm/v/@moq/hang-ui)](https://www.npmjs.com/package/@moq/hang-ui)

SolidJS UI components that interact with hang Web Components.

**Features:**
- Quality selector
- Playback controls
- Chat interface
- Network statistics

[Learn more](/js/@moq/hang-ui)

## Utilities

### @moq/signals

Reactive signals library used by hang for state management.

[Learn more](/js/@moq/signals)

### @moq/clock

Clock utilities for timestamp synchronization.

### @moq/token

JWT token generation and verification for browsers.

[Learn more](/js/@moq/token)

## Installation

```bash
bun add @moq/lite
bun add @moq/hang
bun add @moq/hang-ui

# or with other package managers
npm add @moq/lite
pnpm add @moq/hang
yarn add @moq/hang
```

## Quick Start

### Using Web Components

The easiest way to add MoQ to your web page:

```html
<!DOCTYPE html>
<html>
<head>
    <script type="module">
        import "@moq/hang/publish/element";
        import "@moq/hang/watch/element";
    </script>
</head>
<body>
    <!-- Publish camera/microphone -->
    <hang-publish
        url="https://relay.example.com/anon"
        path="room/alice"
        audio video controls>
        <video muted autoplay></video>
    </hang-publish>

    <!-- Watch the stream -->
    <hang-watch
        url="https://relay.example.com/anon"
        path="room/alice"
        controls>
        <canvas></canvas>
    </hang-watch>
</body>
</html>
```

[Learn more about Web Components](/js/env/web)

### Using JavaScript API

For more control, use the JavaScript API:

```typescript
import * as Moq from "@moq/lite";

// Connect to relay
const connection = await Moq.connect("https://relay.example.com/anon");

// Create and publish a broadcast
const broadcast = new Moq.BroadcastProducer();
const track = broadcast.createTrack("chat");

const group = track.appendGroup();
group.writeString("Hello, MoQ!");
group.close();

connection.publish("my-broadcast", broadcast.consume());
```

[Learn more about @moq/lite](/js/@moq/lite)

## Browser Compatibility

Requires modern browser features:

- **WebTransport** - Chromium-based browsers (Chrome, Edge, Brave)
- **WebCodecs** - For media encoding/decoding
- **WebAudio** - For audio playback

**Supported browsers:**
- Chrome 97+
- Edge 97+
- Brave (recent versions)

**Experimental support:**
- Firefox (behind flag)
- Safari (future support planned)

## Framework Integration

The reactive API works with popular frameworks:

### React

```typescript
import react from "@moq/signals/react";

const publish = document.querySelector("hang-publish") as HangPublish;
const media = react(publish.video.media);

useEffect(() => {
    video.srcObject = media();
}, [media]);
```

### SolidJS

```typescript
import solid from "@moq/signals/solid";

const publish = document.querySelector("hang-publish") as HangPublish;
const media = solid(publish.video.media);

createEffect(() => {
    video.srcObject = media();
});
```

Use `@moq/hang-ui` for ready-made SolidJS components.

## Demo Application

Check out the [hang-demo](https://github.com/moq-dev/moq/tree/main/js/hang-demo) for complete examples:

- Video conferencing
- Screen sharing
- Text chat
- Quality selection

## Next Steps

- Explore [@moq/lite](/js/@moq/lite) - Core protocol
- Explore [@moq/hang](/js/@moq/hang/) - Media library
- Learn about [Web Components](/js/env/web)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)

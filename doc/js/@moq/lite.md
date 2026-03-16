---
title: "@moq/lite"
description: Core pub/sub protocol for browsers
---

# @moq/lite

[![npm](https://img.shields.io/npm/v/@moq/lite)](https://www.npmjs.com/package/@moq/lite)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript implementation of [Media over QUIC](https://moq.dev/) providing real-time data delivery in web browsers. Implements the [moq-lite specification](/spec/draft-lcurley-moq-lite).

## Overview

`@moq/lite` is the browser equivalent of the Rust `moq-lite` crate, providing the core networking layer for MoQ. For higher-level media functionality, use [@moq/hang](/js/@moq/hang/).

## Installation

```bash
bun add @moq/lite
# or
npm add @moq/lite
pnpm add @moq/lite
yarn add @moq/lite
```

## Quick Start

### Basic Connection

```typescript
import * as Moq from "@moq/lite";

// Connect to a MoQ relay server
const connection = await Moq.Connection.connect(
    new URL("https://relay.example.com/anon")
);
console.log("Connected to MoQ relay!");
```

### Publishing Data

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.Connection.connect(
    new URL("https://relay.example.com/anon")
);

// Create a broadcast
const broadcast = new Moq.Broadcast();

// Subscribe to a track (creates it for writing)
const track = broadcast.subscribe("chat", 0);

// Send data in groups
const group = track.appendGroup();
group.writeString("Hello, MoQ!");
group.close();

// Publish to the relay
connection.publish("my-broadcast", broadcast);
```

### Subscribing to Data

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.Connection.connect(
    new URL("https://relay.example.com/anon")
);

// Subscribe to a broadcast
const broadcast = connection.consume("my-broadcast");

// Wait for a track request
const request = await broadcast.requested();
if (request) {
    const track = request.track;

    // Read data as it arrives
    for (;;) {
        const group = await track.nextGroup();
        if (!group) break;

        for (;;) {
            const frame = await group.readString();
            if (!frame) break;

            console.log("Received:", frame);
        }
    }
}
```

### Stream Discovery

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.Connection.connect(
    new URL("https://relay.example.com/anon")
);

// Discover broadcasts announced by the server
const announced = connection.announced();
for (;;) {
    const entry = await announced.next();
    if (!entry) break;

    console.log("Broadcast:", entry.path, entry.active ? "online" : "offline");

    if (entry.active) {
        // Subscribe to the broadcast
        const broadcast = connection.consume(entry.path);
        // ... handle the broadcast
    }
}
```

## Core Concepts

### Broadcasts

A collection of related tracks:

```typescript
const broadcast = new Moq.Broadcast();
```

### Tracks

Named streams within a broadcast, created via `subscribe`:

```typescript
const track = broadcast.subscribe("video", 0);
```

### Groups

Collections of frames (usually aligned with keyframes):

```typescript
const group = track.appendGroup();
group.writeFrame(frameData);
group.close();
```

### Frames

Individual data chunks:

```typescript
// Write raw bytes
group.writeFrame(new Uint8Array([1, 2, 3]));

// Write string (convenience method)
group.writeString("Hello!");
```

## Advanced Usage

### Authentication

Pass JWT tokens via query parameters:

```typescript
const connection = await Moq.Connection.connect(
    new URL(`https://relay.example.com/room/123?jwt=${token}`)
);
```

See [Authentication guide](/app/relay/auth) for details.

### Priority

Set priority when subscribing to a track:

```typescript
const track = broadcast.subscribe("video", 10); // Higher priority
```

## Running Server-Side

`@moq/lite` can also run server-side using a [WebTransport polyfill](https://github.com/fails-components/webtransport):

```typescript
import { WebTransport, quicheLoaded } from "@fails-components/webtransport";
globalThis.WebTransport = WebTransport;

import * as Moq from "@moq/lite";

await quicheLoaded;
const connection = await Moq.Connection.connect(
    new URL("https://relay.example.com/anon")
);
// Same API as browser
```

## TypeScript Support

Full TypeScript support with type definitions:

```typescript
import type { Broadcast, Track } from "@moq/lite";

const broadcast: Broadcast = new Moq.Broadcast();
const track: Track = broadcast.subscribe("video", 0);
```

## Browser Compatibility

Requires **WebTransport** support:

- Chrome 97+
- Edge 97+
- Brave (recent versions)

Firefox and Safari support is experimental or planned.

## Examples

For more examples, see:
- [TypeScript examples](https://github.com/moq-dev/moq/tree/main/js)
- [demo](https://github.com/moq-dev/moq/tree/main/js/demo)

## Protocol Specification

See the [moq-lite specification](/spec/draft-lcurley-moq-lite) for protocol details.

## Next Steps

- Build media apps with [@moq/hang](/js/@moq/hang/)
- Learn about [Web Components](/js/env/web)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Read the [Concepts guide](/concept/)

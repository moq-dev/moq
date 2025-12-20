---
title: "@moq/lite"
description: Core pub/sub protocol for browsers
---

# @moq/lite

[![npm](https://img.shields.io/npm/v/@moq/lite)](https://www.npmjs.com/package/@moq/lite)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript implementation of [Media over QUIC](https://moq.dev/) providing real-time data delivery in web browsers. Implements the [moq-lite specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html).

## Overview

`@moq/lite` is the browser equivalent of the Rust `moq-lite` crate, providing the core networking layer for MoQ. For higher-level media functionality, use [@moq/hang](/typescript/hang).

## Installation

```bash
npm add @moq/lite
# or
pnpm add @moq/lite
bun add @moq/lite
yarn add @moq/lite
```

## Quick Start

### Basic Connection

```typescript
import * as Moq from "@moq/lite";

// Connect to a MoQ relay server
const connection = await Moq.connect("https://relay.example.com/anon");
console.log("Connected to MoQ relay!");
```

### Publishing Data

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.connect("https://relay.example.com/anon");

// Create a broadcast
const broadcast = new Moq.BroadcastProducer();

// Create a track
const track = broadcast.createTrack("chat");

// Send data in groups
const group = track.appendGroup();
group.writeString("Hello, MoQ!");
group.close();

// Publish to the relay
connection.publish("my-broadcast", broadcast.consume());
```

### Subscribing to Data

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.connect("https://relay.example.com/anon");

// Subscribe to a broadcast
const broadcast = connection.consume("my-broadcast");

// Subscribe to a track
const track = await broadcast.subscribe("chat");

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
```

### Stream Discovery

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.connect("https://relay.example.com/anon");

// Discover broadcasts announced by the server
let announcement = await connection.announced();
while (announcement) {
    console.log("New stream available:", announcement.name);

    // Subscribe to the broadcast
    const broadcast = connection.consume(announcement.name);
    // ... handle the broadcast

    announcement = await connection.announced();
}
```

## Core Concepts

### Broadcasts

A collection of related tracks:

```typescript
const broadcast = new Moq.BroadcastProducer();
```

### Tracks

Named streams within a broadcast:

```typescript
const track = broadcast.createTrack("video");
```

### Groups

Collections of frames (usually aligned with keyframes):

```typescript
const group = track.appendGroup();
group.write(frameData);
group.close();
```

### Frames

Individual data chunks:

```typescript
// Write raw bytes
group.write(new Uint8Array([1, 2, 3]));

// Write string (convenience method)
group.writeString("Hello!");
```

## Advanced Usage

### Authentication

Pass JWT tokens via query parameters:

```typescript
const connection = await Moq.connect(
    `https://relay.example.com/room/123?jwt=${token}`
);
```

See [Authentication guide](/guide/authentication) for details.

### Priority

Set priority for groups:

```typescript
const group = track.appendGroup();
group.priority = 10; // Higher priority
group.write(keyframeData);
```

### Error Handling

```typescript
try {
    await connection.publish("my-broadcast", broadcast.consume());
} catch (error) {
    if (error instanceof Moq.ConnectionClosedError) {
        console.error("Connection closed");
    } else if (error instanceof Moq.InvalidPathError) {
        console.error("Invalid path:", error.path);
    } else {
        console.error("Unexpected error:", error);
    }
}
```

### Closing Connections

```typescript
// Close when done
await connection.close();

// Or handle connection close events
connection.addEventListener("close", () => {
    console.log("Connection closed");
});
```

## Running on Deno

`@moq/lite` can also run server-side using [Deno](https://deno.com/):

```typescript
import * as Moq from "npm:@moq/lite";

const connection = await Moq.connect("https://relay.example.com/anon");
// Same API as browser
```

## TypeScript Support

Full TypeScript support with type definitions:

```typescript
import type { Connection, BroadcastProducer, Track } from "@moq/lite";

const connection: Connection = await Moq.connect("...");
const broadcast: BroadcastProducer = new Moq.BroadcastProducer();
const track: Track = broadcast.createTrack("video");
```

## Browser Compatibility

Requires **WebTransport** support:

- Chrome 97+
- Edge 97+
- Brave (recent versions)

Firefox and Safari support is experimental or planned.

## Examples

For more examples, see:
- [TypeScript examples](/typescript/examples)
- [hang-demo](https://github.com/moq-dev/moq/tree/main/js/hang-demo)

## Protocol Specification

See the [moq-lite specification](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html) for protocol details.

## Next Steps

- Build media apps with [@moq/hang](/typescript/hang)
- Learn about [Web Components](/typescript/web-components)
- View [code examples](/typescript/examples)
- Read the [Architecture guide](/guide/architecture)

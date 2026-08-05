---
title: "@moq/net"
description: Real-time pub/sub with caching, fan-out, and prioritization
---

# @moq/net

[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

The networking layer for [Media over QUIC](https://moq.dev/) in TypeScript: real-time pub/sub with built-in caching, fan-out, and prioritization, on top of QUIC. At session setup it negotiates one of two wire protocols: the simplified [moq-lite](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) protocol or the full IETF [moq-transport](https://datatracker.ietf.org/group/moq/documents/) protocol.

## Overview

`@moq/net` is the browser equivalent of the Rust `moq-net` crate, providing the core networking layer for MoQ. For higher-level media functionality, use [@moq/hang](/lib/js/@moq/hang/).

## Installation

```bash
bun add @moq/net
# or
npm add @moq/net
pnpm add @moq/net
yarn add @moq/net
```

## Quick Start

### Basic Connection

See [`js/net/examples/connection.ts`](https://github.com/moq-dev/moq/blob/main/js/net/examples/connection.ts)

### Publishing Data

See [`js/net/examples/publish.ts`](https://github.com/moq-dev/moq/blob/main/js/net/examples/publish.ts)

### Subscribing to Data

See [`js/net/examples/subscribe.ts`](https://github.com/moq-dev/moq/blob/main/js/net/examples/subscribe.ts)

### Stream Discovery

See [`js/net/examples/discovery.ts`](https://github.com/moq-dev/moq/blob/main/js/net/examples/discovery.ts)

## Core Concepts

### Broadcasts

A collection of related tracks.

### Tracks

Named streams within a broadcast, published by the producer and consumed via `subscribe`.

### Groups

Collections of frames (usually aligned with keyframes).

### Frames

Individual data chunks.

See the [publishing example](https://github.com/moq-dev/moq/blob/main/js/net/examples/publish.ts) for usage of all core concepts.

## Advanced Usage

### Remote errors

When a peer resets a stream it sends a numeric code, and a read or write in progress rejects with `Moq.RemoteError` carrying it:

```ts
try {
	frame = await group.readFrame();
} catch (err) {
	if (err instanceof Moq.RemoteError) console.warn("peer reset the group:", err.code);
	throw err;
}
```

The code arrives the same way whether the session negotiated WebTransport or the WebSocket fallback, so nothing has to feature-detect `WebTransportError`.

There are two code registries, and which one applies depends on what failed. A stream reset carries a `Moq.StreamCode`; a session close carries a `Moq.SessionCode`. They are disjoint, so the same number means different things in each: `0` ends a session cleanly but is an internal error on a stream, where a cancellation is `1`. Both tables reuse moq-transport's codes unchanged, and 64 and up are yours.

Anything outside those tables is an unspecified error, so treat it as opaque rather than guessing. That includes 32-63, which the draft reserves: an implementation may send a code there for a condition the shared ones don't cover, but it carries no agreed meaning yet.

A session close surfaces through `connection.closed`, which resolves with `null` for a clean close or a `Moq.RemoteError` when the peer sent a code:

```ts
const err = await connection.closed;
if (err instanceof Moq.RemoteError && err.code === Moq.SessionCode.Unauthorized) {
	console.warn("server rejected the session:", err.message);
}
```

Errors this side detects keep their own messages, like the `Group.Lagged` a read throws after frames were evicted before it got to them.

### Authentication

Pass JWT tokens via query parameters in the URL. See [Authentication guide](/bin/relay/auth) for details and [`js/token/examples/sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/token/examples/sign-and-verify.ts) for a working example.

## Running Server-Side

`@moq/net` can also run server-side using a [WebTransport polyfill](https://github.com/fails-components/webtransport). See the [`js/net/README.md`](https://github.com/moq-dev/moq/blob/main/js/net/README.md#server-side-usage) for setup instructions.

## Browser Compatibility

Requires **WebTransport** support:

- Chrome 97+
- Edge 97+
- Brave (recent versions)

Firefox and Safari support is experimental or planned.

## Examples

For more examples, see:

- [TypeScript examples](https://github.com/moq-dev/moq/tree/main/js)
- [demo](https://github.com/moq-dev/moq/tree/main/demo/web)

## Protocol Specification

See the [moq-lite specification](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) for protocol details.

## Next Steps

- Build media apps with [@moq/hang](/lib/js/@moq/hang/)
- Learn about [Web Components](/lib/js/env/web)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Read the [Concepts guide](/concept/)

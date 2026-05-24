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

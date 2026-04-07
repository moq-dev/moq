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

See [`js/lite/examples/connection.ts`](https://github.com/moq-dev/moq/blob/main/js/lite/examples/connection.ts)

### Publishing Data

See [`js/lite/examples/publish.ts`](https://github.com/moq-dev/moq/blob/main/js/lite/examples/publish.ts)

### Subscribing to Data

See [`js/lite/examples/subscribe.ts`](https://github.com/moq-dev/moq/blob/main/js/lite/examples/subscribe.ts)

### Stream Discovery

See [`js/lite/examples/discovery.ts`](https://github.com/moq-dev/moq/blob/main/js/lite/examples/discovery.ts)

## Core Concepts

### Broadcasts

A collection of related tracks.

### Tracks

Named streams within a broadcast, published by the producer and consumed via `subscribe`.

### Groups

Collections of frames (usually aligned with keyframes).

### Frames

Individual data chunks.

See the [publishing example](https://github.com/moq-dev/moq/blob/main/js/lite/examples/publish.ts) for usage of all core concepts.

## Advanced Usage

### Authentication

Pass JWT tokens via query parameters in the URL. See [Authentication guide](/app/relay/auth) for details and [`js/token/examples/sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/token/examples/sign-and-verify.ts) for a working example.

## Running Server-Side

`@moq/lite` can also run server-side using a [WebTransport polyfill](https://github.com/fails-components/webtransport). See the [`js/lite/README.md`](https://github.com/moq-dev/moq/blob/main/js/lite/README.md#server-side-usage) for setup instructions.

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

See the [moq-lite specification](/spec/draft-lcurley-moq-lite) for protocol details.

## Next Steps

- Build media apps with [@moq/hang](/js/@moq/hang/)
- Learn about [Web Components](/js/env/web)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Read the [Concepts guide](/concept/)

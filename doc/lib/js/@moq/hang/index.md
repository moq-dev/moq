---
title: "@moq/hang"
description: Core media library (catalog, container, support)
---

# @moq/hang

[![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Core media library for [Media over QUIC](https://moq.dev), built on top of [@moq/net](/lib/js/@moq/net). Provides shared primitives used by [`@moq/watch`](/lib/js/@moq/watch) and [`@moq/publish`](/lib/js/@moq/publish).

## Overview

`@moq/hang` provides:

- **Catalog** - JSON track describing other tracks and their codec properties (audio, video, chat, location, etc.)
- **Container** - Media framing in two formats: CMAF (fMP4) and Legacy (varint-timestamp + raw codec bitstream)
- **Utilities** - Hex encoding, Opus audio polyfill (libav), latency computation, browser detection workarounds

Browser support detection is provided by [`<moq-watch-support>`](/lib/js/@moq/watch) and [`<moq-publish-support>`](/lib/js/@moq/publish).

## Installation

```bash
bun add @moq/hang
# or
npm add @moq/hang
pnpm add @moq/hang
```

## JavaScript API

```typescript
import * as Hang from "@moq/hang";

// Catalog — describes tracks and their codec properties
import * as Catalog from "@moq/hang/catalog";

// Container — media framing (CMAF and Legacy formats)
import * as Container from "@moq/hang/container";

// CMAF (fMP4) and Legacy (varint-timestamp + raw bitstream) are both available:
// Container.Cmaf — createVideoInitSegment, createAudioInitSegment, encodeDataSegment, decodeDataSegment, etc.
// Container.Legacy — Producer / Consumer classes
```

For watching and publishing, use the dedicated packages:

```typescript
import * as Watch from "@moq/watch";
import * as Publish from "@moq/publish";
```

## Related Packages

- **[@moq/watch](/lib/js/@moq/watch)** — Subscribe to and render MoQ broadcasts
- **[@moq/publish](/lib/js/@moq/publish)** — Publish media to MoQ broadcasts
- **[@moq/net](/lib/js/@moq/net)** — Core pub/sub transport protocol
- **[@moq/signals](/lib/js/@moq/signals)** — Reactive signals library

## Protocol Specification

See the [hang specification](https://datatracker.ietf.org/doc/draft-lcurley-moq-hang/).

## Next Steps

- Learn about [watching streams](/lib/js/@moq/hang/watch)
- Learn about [publishing streams](/lib/js/@moq/hang/publish)
- Use [Web Components](/lib/js/env/web)
- Use [@moq/net](/lib/js/@moq/net) for custom protocols
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)

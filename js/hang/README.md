<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/hang

[![npm version](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Core media library for [Media over QUIC](https://moq.dev/) (MoQ). Provides shared primitives used by [`@moq/watch`](../watch) and [`@moq/publish`](../publish), built on top of [`@moq/lite`](../lite).

## Features

- **Catalog** — JSON track describing other tracks and their codec properties (audio, video, chat, location, etc.)
- **Container** — Media framing in two formats: CMAF (fMP4) and Legacy (varint-timestamp + raw codec bitstream)
- **Utilities** — Hex encoding, Opus audio polyfill (libav), latency computation, browser detection workarounds

Browser support detection is provided by [`<moq-watch-support>`](../watch) and [`<moq-publish-support>`](../publish).

## Installation

```bash
npm add @moq/hang
# or
pnpm add @moq/hang
yarn add @moq/hang
bun add @moq/hang
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

- **[@moq/watch](../watch)** — Subscribe to and render MoQ broadcasts
- **[@moq/publish](../publish)** — Publish media to MoQ broadcasts
- **[@moq/ui-core](../ui-core)** — Shared UI components
- **[@moq/lite](../lite)** — Core pub/sub transport protocol
- **[@moq/signals](../signals)** — Reactive signals library

## License

Licensed under either:

-   Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

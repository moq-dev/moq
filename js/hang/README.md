<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/hang

[![npm version](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript library providing core media components for [Media over QUIC](https://moq.dev/) (MoQ).

**`@moq/hang`** provides catalog schemas, container encoding/decoding, and browser support detection for MoQ media streaming.

> **Note:** For publishing and watching media streams, see [`@moq/publish`](../publish) and [`@moq/watch`](../watch).

## Installation

```bash
npm add @moq/hang
# or
bun add @moq/hang
```

## Features

- **Catalog**: JSON schema definitions for media metadata (video, audio, user, chat, location, preview)
- **Container**: Frame encoding/decoding for both legacy and CMAF/fMP4 formats
- **Support**: Browser capability detection for WebTransport, WebCodecs, and media codecs

## Usage

### Catalog

```typescript
import * as Catalog from "@moq/hang/catalog";

// Encode a catalog
const encoded = Catalog.encode({
    video: { renditions: { "video/hd": videoConfig } },
    audio: { renditions: { "audio/data": audioConfig } },
});

// Decode a catalog
const decoded = Catalog.decode(buffer);
```

### Container

```typescript
import * as Container from "@moq/hang/container";

// Legacy format - encode/decode frames with timestamps
const producer = new Container.Legacy.Producer(track);
producer.encode(chunk, timestamp, isKeyframe);

// CMAF format - create fMP4 segments
const init = Container.Cmaf.createVideoInitSegment(config);
const data = Container.Cmaf.encodeDataSegment(frame, timescale);
```

### Support Detection

```typescript
import { isSupported } from "@moq/hang/support";

const support = await isSupported();
console.log("WebTransport:", support.webtransport);
console.log("Video encoding:", support.video.encoding);
```

### `<hang-support>` Web Component

```html
<script type="module">
    import "@moq/hang/support/element";
</script>

<!-- Show browser support status -->
<hang-support mode="publish" show="partial" />
```

## Related Packages

- [`@moq/publish`](../publish) - Publishing media streams
- [`@moq/watch`](../watch) - Watching media streams
- [`@moq/publish-ui`](../publish-ui) - Publishing UI components
- [`@moq/watch-ui`](../watch-ui) - Watching UI components
- [`@moq/lite`](../lite) - Core MoQ protocol implementation

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

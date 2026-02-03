<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/watch

[![npm version](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript library for watching real-time media streams using [Media over QUIC](https://moq.dev/) (MoQ).

**`@moq/watch`** provides high-level components for consuming live audio and video streams, built on top of [`@moq/lite`](../lite) and [`@moq/hang`](../hang).

## Installation

```bash
npm add @moq/watch
# or
bun add @moq/watch
```

## Web Component

```html
<script type="module">
    import "@moq/watch/element";
</script>

<hang-watch
    url="https://cdn.moq.dev/anon"
    path="room123/me"
    controls>
    <!-- canvas for rendering, otherwise video element will be disabled -->
    <canvas></canvas>
</hang-watch>
```

### Attributes

- `url` (required): The URL of the server, potentially authenticated via a `?jwt` token.
- `path` (required): The path of the broadcast.
- `controls`: Show simple playback controls.
- `paused`: Pause playback.
- `muted`: Mute audio playback.
- `volume`: Set the audio volume, only when `!muted`.

## JavaScript API

```typescript
import * as Moq from "@moq/lite";
import { Broadcast } from "@moq/watch";

// Create a new connection
const connection = new Moq.Connection("https://cdn.moq.dev/anon");

// Subscribing to media
const watch = new Broadcast({
    connection: connection.established,
    enabled: true,
    path: "bob",
});

// Access the catalog
watch.catalog.subscribe((catalog) => {
    console.log("Catalog updated:", catalog);
});
```

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

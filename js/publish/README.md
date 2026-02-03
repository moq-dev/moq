<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/publish

[![npm version](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript library for publishing real-time media streams using [Media over QUIC](https://moq.dev/) (MoQ).

**`@moq/publish`** provides high-level components for publishing live audio and video streams, built on top of [`@moq/lite`](../lite) and [`@moq/hang`](../hang).

## Installation

```bash
npm add @moq/publish
# or
bun add @moq/publish
```

## Web Component

```html
<script type="module">
    import "@moq/publish/element";
</script>

<hang-publish
    url="https://cdn.moq.dev/anon"
    path="room123/me"
    audio
    video
    controls>
    <!-- Optional: video element for preview -->
    <video autoplay muted></video>
</hang-publish>
```

### Attributes

- `url` (required): The URL of the server, potentially authenticated via a `?jwt` token.
- `path` (required): The path of the broadcast.
- `source`: "camera", "screen", or "file".
- `audio`: Enable audio capture.
- `video`: Enable video capture.
- `controls`: Show simple publishing controls.

## JavaScript API

```typescript
import * as Moq from "@moq/lite";
import { Broadcast } from "@moq/publish";

// Create a new connection
const connection = new Moq.Connection("https://cdn.moq.dev/anon");

// Publishing media
const publish = new Broadcast({
    connection: connection.established,
    enabled: true,
    path: "bob",
});

// Configure video/audio
publish.video.source.set(mediaStreamTrack);
publish.audio.source.set(audioStreamTrack);
```

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

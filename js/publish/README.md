<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/publish

[![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Publish media to [Media over QUIC](https://moq.dev/) (MoQ) broadcasts, built on top of [@moq/hang](../hang) and [@moq/lite](../lite).

## Installation

```bash
bun add @moq/publish
# or
npm add @moq/publish
```

## Web Component

The simplest way to publish a stream:

```html
<script type="module">
    import "@moq/publish/element";
</script>

<moq-publish
    url="https://relay.example.com/anon"
    path="room/alice"
    audio video controls>
    <video muted autoplay></video>
</moq-publish>
```

### Attributes

| Attribute  | Type    | Default  | Description                     |
|------------|---------|----------|---------------------------------|
| `url`      | string  | required | Relay server URL                |
| `path`     | string  | required | Broadcast path                  |
| `source`   | string  | —        | `"camera"`, `"screen"`, `"file"` |
| `audio`    | boolean | false    | Enable audio capture            |
| `video`    | boolean | false    | Enable video capture            |
| `controls` | boolean | false    | Show simple publishing controls |

## JavaScript API

For more control:

```typescript
import * as Publish from "@moq/publish";

const publish = new Publish.Broadcast(connection, {
    enabled: true,
    name: "alice",
    video: { enabled: true },
    audio: { enabled: true },
});

// Change source at runtime
publish.source.camera.enabled.set(true);
```

## UI Web Component

`@moq/publish` includes a SolidJS-powered UI overlay (`<moq-publish-ui>`) with source selection (camera, screen, file, microphone) and status indicator. It depends on [`@moq/ui-core`](../ui-core) for shared UI primitives.

```html
<script type="module">
    import "@moq/publish/element";
    import "@moq/publish/ui";
</script>

<moq-publish-ui>
    <moq-publish url="https://relay.example.com/anon" path="room/alice" audio video>
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

The `<moq-publish-ui>` element automatically discovers the nested `<moq-publish>` element and wires up reactive controls.

## Features

- **Camera & microphone** — Capture from user devices
- **Screen sharing** — Capture display or window
- **File playback** — Publish from a media file
- **WebCodecs encoding** — Hardware-accelerated video and audio encoding
- **Reactive state** — All properties are signals from `@moq/signals`
- **Chat** — Publish text chat messages
- **Location** — Publish peer position and window tracking

## License

Licensed under either:

-   Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)

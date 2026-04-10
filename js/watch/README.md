<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Subscribe to and render [Media over QUIC](https://moq.dev/) (MoQ) broadcasts, built on top of [@moq/hang](../hang) and [@moq/lite](../lite).

## Installation

```bash
bun add @moq/watch
# or
npm add @moq/watch
```

### Without a bundler (CDN)

The package ships a pre-built, self-contained ESM bundle under `bundle/`. Drop
it in directly from jsDelivr (or any other npm-backed CDN — unpkg, esm.sh, etc.)
and the `<moq-watch>`, `<moq-watch-ui>`, and `<moq-watch-support>` custom
elements are registered on load:

```html
<script type="module"
    src="https://cdn.jsdelivr.net/npm/@moq/watch/bundle/moq-watch.js"></script>

<moq-watch-ui>
    <moq-watch url="https://relay.example.com/anon" name="room/alice">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

Pin a specific version (recommended for production) with a version range in the
URL, e.g. `https://cdn.jsdelivr.net/npm/@moq/watch@0.2/bundle/moq-watch.js`.

The bundle inlines `@moq/lite`, `@moq/hang`, `@moq/signals`, `@moq/ui-core`,
SolidJS and the WebCodecs/Opus fallbacks — no additional network requests are
needed and no import map has to be configured.

## Web Component

The simplest way to watch a stream when using a bundler:

```html
<script type="module">
    import "@moq/watch/element";
</script>

<moq-watch
    url="https://relay.example.com/anon"
    path="room/alice"
    controls>
    <canvas></canvas>
</moq-watch>
```

### Attributes

| Attribute | Type    | Default  | Description           |
|-----------|---------|----------|-----------------------|
| `url`     | string  | required | Relay server URL      |
| `path`    | string  | required | Broadcast path        |
| `paused`  | boolean | false    | Pause playback        |
| `muted`   | boolean | false    | Mute audio            |
| `volume`  | number  | 1        | Audio volume (0-1)    |

## JavaScript API

For more control:

```typescript
import * as Watch from "@moq/watch";

const watch = new Watch.Broadcast(connection, {
    enabled: true,
    name: "alice",
    video: { enabled: true },
    audio: { enabled: true },
});

// Access the video stream
watch.video.media.subscribe((stream) => {
    if (stream) {
        videoElement.srcObject = stream;
    }
});
```

## UI Web Component

`@moq/watch` includes a SolidJS-powered UI overlay (`<moq-watch-ui>`) with playback controls, volume, buffering indicator, quality selector, and stats panel. It depends on [`@moq/ui-core`](../ui-core) for shared UI primitives.

```html
<script type="module">
    import "@moq/watch/element";
    import "@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch url="https://relay.example.com/anon" path="room/alice">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

The `<moq-watch-ui>` element automatically discovers the nested `<moq-watch>` element and wires up reactive controls.

## Features

- **WebCodecs decoding** — Hardware-accelerated video and audio decoding
- **MSE fallback** — Media Source Extensions for broader codec support
- **Reactive state** — All properties are signals from `@moq/signals`
- **Chat** — Subscribe to text chat channels
- **Location** — Peer location and window tracking
- **Quality selection** — Switch between available renditions

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)

<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Subscribe to and render [Media over QUIC](https://moq.dev/) (MoQ) broadcasts, built on top of [@moq/hang](../hang) and [@moq/net](../lite).

## Installation

```bash
bun add @moq/watch
# or
npm add @moq/watch
```

### No-build CDN usage

For quick demos or embeds where a bundler is overkill, esm.sh serves the
published npm package as a browser-ready ESM module. Bare imports like
`@moq/hang` are automatically rewritten to other esm.sh URLs. No build step or
import map required:

```html
<script type="module">
    import "https://esm.sh/@moq/watch/element";
    import "https://esm.sh/@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch url="https://relay.example.com/anon" name="room/alice.hang">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

Pin a version range in the URL for production, e.g.
`https://esm.sh/@moq/watch@0.2/element`. jsDelivr's `+esm` endpoint
(`https://cdn.jsdelivr.net/npm/@moq/watch/element.js/+esm`) works the same way
if you prefer it.

For anything beyond embedding on a static page you should install the
package and use a real bundler (the examples below).

## Web Component

The simplest way to watch a stream:

```html
<script type="module">
    import "@moq/watch/element";
</script>

<moq-watch
    url="https://relay.example.com/anon"
    name="room/alice.hang"
    controls>
    <canvas></canvas>
</moq-watch>
```

### Attributes

| Attribute | Type                                | Default  | Description                              |
|-----------|-------------------------------------|----------|------------------------------------------|
| `url`     | string                              | required | Relay server URL                         |
| `name`    | string                              | required | Broadcast name/path                      |
| `paused`  | boolean                             | false    | Pause playback                           |
| `muted`   | boolean                             | false    | Mute audio                               |
| `visible` | never, distance, or always          | `0px`    | When to download video (see below)       |
| `volume`  | number                              | 0.5      | Audio volume (0-1)                       |
| `reload`  | boolean                             | true     | Wait for (re)announcement before subscribing. Defaults off for `mediaoverquic.com` relays until they support broadcast discovery. |

The `visible` attribute controls when the video track is downloaded, based on the canvas
position relative to the viewport:

- `never`: never download video.
- a distance (`0px`, `200px`, `100%`, ...): download while the canvas is within that distance
  of the viewport **and** the tab is visible. `0px` (the default) means strictly on screen; a
  larger distance pre-warms the video before it scrolls into view.
- `always`: always download video, regardless of the canvas position or tab visibility.

Only the distance mode suspends video while the tab is hidden; `always` keeps downloading.

## JavaScript API

For more control:

```typescript
import * as Watch from "@moq/watch";

const watch = new Watch.Broadcast(connection, {
    enabled: true,
    name: "alice.hang",
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

`@moq/watch` includes a Web Component UI overlay (`<moq-watch-ui>`) with playback controls, volume, buffering indicator, quality selector, and stats panel. It is built on top of `@moq/signals` with no framework dependency.

```html
<script type="module">
    import "@moq/watch/element";
    import "@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch url="https://relay.example.com/anon" name="room/alice.hang">
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

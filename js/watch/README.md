<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Subscribe to and render [Media over QUIC](https://moq.dev/) (MoQ) broadcasts, built on top of [@moq/hang](../hang) and [@moq/net](../net).

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

<moq-watch url="https://relay.example.com/anon" name="room/alice.hang">
    <canvas></canvas>
</moq-watch>
```

### Attributes

| Attribute        | Type                       | Default       | Description                              |
|------------------|----------------------------|---------------|------------------------------------------|
| `url`            | string                     | required      | Relay server URL                         |
| `name`           | string                     | required      | Broadcast name/path                      |
| `paused`         | boolean                    | false         | Pause playback                           |
| `muted`          | boolean                    | false         | Mute audio                               |
| `visible`        | never, distance, or always | `20%`         | When to download video (see below)       |
| `volume`         | number                     | 0.5           | Audio volume (0-1)                       |
| `announced`      | boolean                    | true          | Wait for (re)announcement before subscribing. Ignored when the relay does not support broadcast discovery. |
| `delay`          | `auto`, duration, `instant` | `auto`       | Distance from the live edge. `instant` paints frames as they decode and disables audio. |
| `buffer`         | duration                   | `0ms`         | Future-dated media held before playback skips ahead. |
| `captions`       | string                     | off           | Text rendition to render. |
| `catalog-format` | hang, hangz, msf, manual   | auto-detected | The catalog format; detected from the name suffix unless set. `hangz` (compressed) is opt-in. |

The `visible` attribute controls when the video track is downloaded, based on the canvas
position relative to the viewport:

- `never`: never download video.
- a distance (`0px`, `200px`, `100%`, ...): download while the canvas is within that distance
  of the viewport **and** the tab is visible. `0px` means strictly on screen; a larger distance
  (`20%`, the default) pre-warms the video before it scrolls into view.
- `always`: always download video, regardless of the canvas position or tab visibility.

Only the distance mode suspends video while the tab is hidden; `always` keeps downloading.

## JavaScript API

For a headless player, construct `Player` with a connection origin and a canvas.
It owns the broadcast, rendition selection, synchronized decoders, video
renderer, audio emitter, and captions. Pass `Signal` values to change controls
later; call `close()` when playback ends.

```typescript
import * as Watch from "@moq/watch";
import { Signal } from "@moq/signals";

const connection = new Watch.Net.Connection({
    url: new URL("https://relay.example.com/anon"),
    enabled: true,
});
const muted = new Signal(false);
const player = new Watch.Player({
    origin: connection.origin,
    probe: connection.probe,
    name: Watch.Net.Path.from("room/alice.hang"),
    canvas,
    muted,
});

// player.broadcast, player.video, player.audio, player.text,
// player.renderer, player.emitter, and player.sync expose the pipeline.
// Later: player.close(); connection.close();
```

`<moq-watch>` wraps this same `Player` and maps attributes to its controls.
`Broadcast`, `Sync`, and the `Video`, `Audio`, and `Text` components remain
available when an application needs a different pipeline.

## UI Web Component

`@moq/watch` includes a Web Component UI overlay (`<moq-watch-ui>`) with playback controls, volume, buffering indicator, unsupported-codec indicator, quality selector, and stats panel. It is built on top of `@moq/signals` with no framework dependency.

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

- **WebCodecs decoding**: Hardware-accelerated video and audio decoding
- **Reactive state**: All properties are signals from `@moq/signals`
- **Latency control**: A delay target plus optional buffering for future-dated frames
- **Quality selection**: Switch between available renditions
- **Custom tracks**: Unknown catalog sections pass through, and `broadcast.out.active` subscribes your own tracks

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)

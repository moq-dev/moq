<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/publish

[![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Publish media to [Media over QUIC](https://moq.dev/) (MoQ) broadcasts, built on top of [@moq/hang](../hang) and [@moq/net](../net).

## Installation

```bash
bun add @moq/publish
# or
npm add @moq/publish
```

### No-build CDN usage

For quick demos or embeds where a bundler is overkill, esm.sh serves the
published npm package as a browser-ready ESM module. Bare imports like
`@moq/hang` are automatically rewritten to other esm.sh URLs. No build step or
import map required:

```html
<script type="module">
    import "https://esm.sh/@moq/publish/element";
    import "https://esm.sh/@moq/publish/ui";
</script>

<moq-publish-ui>
    <moq-publish url="https://relay.example.com/anon" name="room/alice.hang" source="camera">
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

Pin a version range in the URL for production, e.g.
`https://esm.sh/@moq/publish@0.2/element`. jsDelivr's `+esm` endpoint
(`https://cdn.jsdelivr.net/npm/@moq/publish/element.js/+esm`) works the same way
if you prefer it.

For anything beyond embedding on a static page you should install the
package and use a real bundler (the examples below).

## Web Component

The simplest way to publish a stream:

```html
<script type="module">
    import "@moq/publish/element";
</script>

<moq-publish url="https://relay.example.com/anon" name="room/alice.hang" source="camera">
    <video muted autoplay></video>
</moq-publish>
```

### Attributes

| Attribute   | Type    | Default  | Description                     |
|-------------|---------|----------|---------------------------------|
| `url`       | string  | required | Relay server URL                |
| `name`      | string  | required | Broadcast name                  |
| `source`    | string  | —        | `"camera"`, `"screen"`, `"file"` |
| `muted`     | boolean | false    | Mute audio capture              |
| `invisible` | boolean | false    | Disable video capture           |
| `preview`   | string  | `"source"` | What the preview renders: `"source"`, `"encoded"`, `"none"` |
| `announce`  | string  | `"source"` | When to publish: `"always"`, `"never"`, `"source"` (once media is actually captured) |

A nested `<video>` shows the raw capture; a `<canvas>` is drawn by the element.

## JavaScript API

For more control, `Broadcast` owns the network broadcast and the catalog. Renditions are
registered by the encoders themselves, one per track name, so simulcast is just
more encoders:

```typescript
import * as Publish from "@moq/publish";

const connection = new Publish.Net.Connection.Reload({
    url: new URL("https://relay.example.com/anon"),
    enabled: true,
});

const broadcast = new Publish.Broadcast({
    connection: connection.established,
    enabled: true,
    name: Publish.Net.Path.from("room/alice.hang"),
});

// Capture, then encode. Each encoder registers its rendition on the broadcast
// and encodes only while someone is subscribed.
const camera = new Publish.Source.Camera({ enabled: true });
const capture = new Publish.Video.Capture({ source: camera.out.source });

const hd = new Publish.Video.Encoder("video/hd", { broadcast, capture, enabled: true });
const sd = new Publish.Video.Encoder("video/sd", { broadcast, capture, enabled: true, config: { maxScale: 0.25 } });

// Tune an encoder at any time; undefined fields are auto-sized.
hd.config.set({ codec: "vp09.00.10.08", maxBitrate: 4_000_000 });

const microphone = new Publish.Source.Microphone({ enabled: true });
const audio = new Publish.Audio.Encoder("audio", { broadcast, source: microphone.out.source, enabled: true });
audio.volume.set(0.8);
```

Serve application tracks alongside the media with `broadcast.net`, the
underlying producer, and advertise them with `broadcast.catalog.mutate(...)`.

## UI Web Component

`@moq/publish` includes a Web Component UI overlay (`<moq-publish-ui>`) with source selection (camera, screen, file, microphone), audio/video toggles, a status badge, fullscreen, and a stats panel. It is built on top of `@moq/signals` with no framework dependency.

```html
<script type="module">
    import "@moq/publish/element";
    import "@moq/publish/ui";
</script>

<moq-publish-ui>
    <moq-publish url="https://relay.example.com/anon" name="room/alice.hang" source="camera">
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
- **Simulcast** — Any number of renditions, each its own encoder and track
- **Custom tracks** — Application tracks and catalog sections ride alongside the media

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)

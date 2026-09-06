---
title: "@moq/publish"
description: Capture, encode, and publish from the browser
---

# @moq/publish

[![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish)

The publisher: captures a camera, microphone, screen, or file, encodes with
WebCodecs, writes the catalog, and publishes a hang broadcast.

```html
<script type="module">
    import "@moq/publish/element";
    import "@moq/publish/ui";     // optional device picker and controls
</script>

<moq-publish-ui>
    <moq-publish url="https://relay.example.com/anon" name="room/alice.hang" source="camera">
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

## Attributes

| Attribute | |
| --- | --- |
| `url`, `name` | Relay URL (with `?jwt=` if needed) and broadcast name. |
| `source` | `camera`, `screen`, or `file`. |
| `muted`, `invisible` | Disable audio or video capture. |
| `preview` | What the nested element shows: the raw `source` (default), a decoded copy of the `encoded` stream to see what viewers get, or `none`. |
| `announce` | When to announce: once a `source` is live (default), `always`, or `never`. |

A nested `<video>` gets the raw capture stream; a `<canvas>` is drawn by the
element. `<moq-publish-support>` shows what the browser can encode.

## Encoding

The video encoder's bitrate cap follows the connection's bandwidth estimate,
so a tightening uplink costs quality instead of stalling. Codec, resolution,
framerate, and bitrate are tunable through `el.video.config`; the audio
encoder exposes its codec and volume. For simulcast or several renditions,
drop the element and register your own encoders on a `Publish.Broadcast`.

## Custom tracks

`broadcast.publishTrack(name, serve)` serves any application track per
subscriber, and `broadcast.catalog.mutate(c => { c.yourSection = ... })`
advertises it without touching the media sections. Encode JSON with
`@moq/json`.

## Without the element

```ts
import * as Publish from "@moq/publish";

const broadcast = new Publish.Broadcast({
    connection,
    enabled: true,
    name: "alice.hang",
    video: { hd: { enabled: true }, sd: { enabled: true } },   // two renditions
    audio: { enabled: true },
});
```

Every input and output is a signal from [`@moq/signals`](/lib/js/signals).
Load from a CDN (`https://esm.sh/@moq/publish/element`) for a no-build embed.

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
| `announce` | When to advertise: once the `source` is live (default), `always`, or `never`. Nobody can see or subscribe to the broadcast until it is announced. |

Every attribute is also a reactive property. The
[README](https://www.npmjs.com/package/@moq/publish) lists types and defaults.
A nested `<video>` gets the raw capture stream; a `<canvas>` is drawn by the
element. `<moq-publish-support>` shows what the browser can encode.

Capture failures, such as `NotAllowedError` when permission is refused, land in
`out.error` on the camera or microphone in `el.sources.video` and
`el.sources.audio`, and `<moq-publish-ui>` shows them. A refused capture waits
for a permission, device, or settings change instead of prompting again.

## Encoding

The video encoder follows its share of the connection's send-rate estimate, so
several publishers on one connection do not each target the whole uplink.
Codec, resolution, framerate, and bitrate are tunable through
`el.video.config`, and `el.video.cut()` asks for an extra keyframe. For
simulcast, drop the element and register several encoders on a
`Publish.Broadcast`, as below.

Timestamps are `performance.now()` in microseconds, so every source shares one
timeline. The catalog's `clock` maps it to wall time. Stamp your own tracks
(e.g. text cues) on the same timeline to stay in sync.

## Custom tracks

`broadcast.net` is the underlying `Moq.Broadcast.Producer`, so an application
can serve its own tracks alongside the media. It is recreated on each
reconnect, so acquire it from an effect and reseed the track each time:

```ts
import * as Json from "@moq/json";

signals.run((effect) => {
    const net = effect.get(broadcast.net);
    if (!net) return;

    // A day-long retention so a late viewer still replays the last value.
    const track = net.createTrack("meta.json", { maxAge: 86_400_000 });
    effect.cleanup(() => track.close());

    const meta = new Json.Snapshot.Producer<Meta>({ track });
    meta.update(current);
});
```

Advertise it in the catalog without touching the media sections. The schema
is loose, so cast to name your own section:

```ts
broadcast.catalog.mutate((catalog) => {
    (catalog as Catalog.Root & { metadata?: string[] }).metadata = ["meta.json"];
});
```

## Without the element

```ts
import * as Moq from "@moq/net";
import * as Publish from "@moq/publish";

// Shared with every other component pointed at the same relay; its origin holds
// the broadcasts, so they survive a reconnect.
const connection = new Moq.Connection({ url: new URL("https://relay.example.com/anon") });

const broadcast = new Publish.Broadcast({
    origin: connection.origin,
    enabled: true,
    name: Publish.Net.Path.from("alice.hang"),
});

const camera = new Publish.Source.Camera({ enabled: true });
const microphone = new Publish.Source.Microphone({ enabled: true });
const video = new Publish.Signals.Computed((effect) => effect.get(camera.out.source)?.video);
const capture = new Publish.Video.Capture({ source: video });

// Each encoder registers a rendition on the broadcast (`broadcast.video(name)`) and
// encodes only while someone is subscribed.
new Publish.Video.Encoder("video/hd", { broadcast, capture, enabled: true });
new Publish.Video.Encoder("video/sd", { broadcast, capture, enabled: true, config: { maxScale: 0.25 } });
const audioSource = new Publish.Signals.Computed((effect) => effect.get(microphone.out.source)?.audio);
const audioCapture = new Publish.Audio.Capture({ source: audioSource });
new Publish.Audio.Encoder("audio", { broadcast, capture: audioCapture, enabled: true });
```

Standalone components start enabled unless you pass `enabled: false` (or a
signal). An enabled camera or microphone prompts for permission on
construction, and a screen source must be built inside the user gesture that
authorizes it. Audio waits for the page's first click or keypress, since
browsers suspend Web Audio until then. Every input and output is a signal from
[`@moq/signals`](/lib/js/signals).

## Strict CSP

The audio worklet and the capture worker load from `blob:` URLs by default, so
a CSP must allow `blob:` in `script-src` and `worker-src`. Otherwise, copy
`node_modules/@moq/publish/assets/*` into a directory your origin serves and
point the package at it before capture starts:

```ts
import * as Publish from "@moq/publish";

Publish.assets("/moq/");
```

The URL must end with `/`. Copy the files again on every upgrade: they change
with the package. `@moq/room` publishes through `@moq/publish`, so this one
call covers it.

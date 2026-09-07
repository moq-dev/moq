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

`broadcast.net` is the underlying `Moq.Broadcast.Producer`, so an application
can serve its own tracks alongside the media. It is recreated on each
(re)connection, so acquire it from an effect and reseed the track each time:

```ts
import * as Json from "@moq/json";

signals.run((effect) => {
    const net = effect.get(broadcast.net);
    if (!net) return;

    // A day-long retention so a late viewer still replays the last value.
    const track = net.createTrack("meta.json", { latencyMax: 86_400_000 });
    effect.cleanup(() => track.close());

    const meta = new Json.Snapshot.Producer<Meta>({ track });
    meta.update(current);
});
```

`broadcast.catalog.mutate(c => { c.yourSection = ... })` advertises it without
touching the media sections, which are folded in from the registered
renditions. The catalog schema is loose, so an unknown root section passes
through untouched; cast to name your own:

```ts
broadcast.catalog.mutate((catalog) => {
    (catalog as Catalog.Root & { metadata?: string[] }).metadata = ["meta.json"];
});
```

## Without the element

```ts
import * as Publish from "@moq/publish";

const broadcast = new Publish.Broadcast({
    connection,                                   // a Net.Connection.Established signal
    enabled: true,
    name: Publish.Net.Path.from("alice.hang"),
});

const camera = new Publish.Source.Camera({ enabled: true });
const microphone = new Publish.Source.Microphone({ enabled: true });
const capture = new Publish.Video.Capture({ source: camera.out.source });

// Each encoder registers a rendition on the broadcast (`broadcast.video(name)`) and
// encodes only while someone is subscribed.
new Publish.Video.Encoder("video/hd", { broadcast, capture, enabled: true });
new Publish.Video.Encoder("video/sd", { broadcast, capture, enabled: true, config: { maxScale: 0.25 } });
new Publish.Audio.Encoder("audio", { broadcast, source: microphone.out.source, enabled: true });
```

Every input and output is a signal from [`@moq/signals`](/lib/js/signals).
Load from a CDN (`https://esm.sh/@moq/publish/element`) for a no-build embed.

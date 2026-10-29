---
title: "@moq/publish"
description: Capture, encode, and publish from the browser
---

# @moq/publish

[![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish)
[![API](https://img.shields.io/badge/API-reference-blue)](/lib/js/api/publish)

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
| `versions` | Protocol versions to offer, most preferred first, comma-separated like `moq-transport-18,moq-lite-06`. Every stable version by default. |
| `source` | `camera`, `screen`, or `file`. |
| `muted`, `invisible` | Disable audio or video capture. |
| `preview` | What the nested element shows: the raw `source` (default), a decoded copy of the `encoded` stream to see what viewers get, or `none`. |
| `backend` | How a `<canvas>` preview is drawn; see [@moq/video](/lib/js/video). `auto` (default), `webgpu`, or `2d`. |
| `announce` | When to advertise: once the `source` is live (default), or `always`. Nobody can see or subscribe to the broadcast until it is announced. To keep it out of discovery, use a hidden name with a `.`-prefixed segment; it is still subscribable by name. |

Every attribute is also a reactive property, and property or UI changes reflect
back into the attributes. An invalid value is refused rather than defaulted: the
property setter throws, and invalid markup reports a browser `error` event and
suspends the element until corrected or removed. The
[README](https://www.npmjs.com/package/@moq/publish) lists types and defaults.
A nested `<video>` gets the raw capture stream; a `<canvas>` is drawn by the
element. `<moq-publish-support>` shows what the browser can encode.

Capture failures, such as `NotAllowedError` when permission is refused, land in
`out.error` on the camera or microphone in `el.sources.video` and
`el.sources.audio`, and `<moq-publish-ui>` shows them. A refused capture waits
for a permission, device, or settings change instead of prompting again.

## Encoding

An enabled encoder encodes whether or not anyone is subscribed, so its recent
groups wait in the track's cache: a viewer who subscribes late with a
`maxDelay`, or fetches, reaches back as far as `maxAge` keeps. Pass
`onDemand: true` to `Publish.Broadcast` to encode every rendition only while
someone is subscribed instead. That saves the encode while nobody watches, but
a viewer then starts at the keyframe its arrival forces, with no past to reach.
Bandwidth is reserved only while someone is subscribed, so an unwatched
rendition encodes at its configured bitrate.

The video encoder follows its share of the connection's send-rate estimate, so
several publishers on one connection do not each target the whole uplink.
Codec, resolution, framerate, and bitrate are tunable through
`el.video.config`, and `el.video.cut()` asks for an extra keyframe. A viewer
who joins once a group's deltas outweigh twice its keyframe gets a fresh one,
so `keyframeInterval` is only the longest a group runs. Through a moq-lite
relay, only the join that opens the relay's upstream subscription counts. Size,
bitrate, and framerate change in place, while a codec or audio format change
keeps the rendition's catalog ID under a fresh `track` name that viewers
follow through the catalog. For
simulcast, drop the element and register several encoders on a
`Publish.Broadcast`, as below.

Audio groups carry at least 20 ms by default, packing shorter codec frames
together. `el.audio.groupDuration` changes that minimum; `Milli(0)` puts
every frame in its own group. A longer minimum lets a relay keep fewer
streams, at the cost of coarser loss: a viewer that falls behind skips a whole
group. Frames still forward as they are encoded.

Timestamps are `@moq/clock`'s `now()` in microseconds, so every source shares one
timeline. The catalog's `clock` maps it to wall time. Stamp your own tracks
(e.g. text cues) on the same timeline to stay in sync.

## Custom tracks

`broadcast.net` is the underlying `Moq.Broadcast.Producer`, so an application
can serve its own tracks alongside the media. It is recreated when the origin, name,
`enabled`, or `maxAge` changes, so acquire it from an effect and create each
track once per `net`. Names under a rendition's ID, such as `video/1`, are
reserved for that rendition's replacement tracks. A track name always means
the same content, so a track re-created on the same `net` throws on an append
that would reuse its sequences:

```ts
import { Milli } from "@moq/clock";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";

signals.run((effect) => {
    const net = effect.get(broadcast.net);
    if (!net) return;

    // A day-long retention so a late viewer still replays the last value. Each
    // value is stamped with when it was written, so the track declares a timescale.
    const track = net.createTrack("meta.json", {
        timescale: Moq.Time.Timescale.MILLI,
        maxAge: Milli(86_400_000),
    });
    effect.cleanup(() => {
        track.finish();
        track.close();
    });

    const meta = new Json.Snapshot.Producer<Meta>({ track });
    meta.update({ value: current, at: Moq.Time.Timestamp.now() });
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

const camera = new Publish.Source.Camera({ enabled: true });
const microphone = new Publish.Source.Microphone({ enabled: true });
const video = new Publish.Signals.Computed((effect) => effect.get(camera.out.source)?.video);
// The orientation turns a phone's landscape frames upright while it is held in portrait.
const capture = new Publish.Video.Capture({ source: video, orientation: camera.out.orientation });

const broadcast = new Publish.Broadcast({
    origin: connection.origin,
    enabled: true,
    name: Publish.Net.Path.from("alice.hang"),
    display: capture.out.display,
    rotation: capture.out.rotation,
});

// Each encoder registers a rendition on the broadcast (`broadcast.video(name)`) and
// encodes while enabled, subscribed or not. Pass `onDemand: true` to the broadcast
// to encode only while someone is subscribed.
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
[`@moq/signals`](/lib/js/signals). A failed encoder exposes the cause through
`out.error` and disables only its rendition; siblings and the broadcast keep
running. It does not retry on demand or bandwidth changes, and enable cycles
that keep the same capture do not retry. A new capture source (such as the
track a camera or microphone reopens on unmute), changed encoder settings, or
a replacement encoder recovers it. Recovery keeps the catalog ID and uses a
fresh `track` name.

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

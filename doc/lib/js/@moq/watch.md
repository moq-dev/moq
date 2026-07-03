---
title: "@moq/watch"
description: Subscribe to and render MoQ broadcasts
---

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Subscribe to and render MoQ broadcasts. Provides both a JavaScript API and a `<moq-watch>` Web Component, plus an optional `<moq-watch-ui>` overlay.

## Installation

```bash
bun add @moq/watch
# or
npm add @moq/watch
```

### No-build CDN usage

For quick demos or single-page embeds where a bundler is overkill, load the
package straight from [esm.sh](https://esm.sh). esm.sh serves the package as a
browser-ready ESM module and rewrites bare imports (like `@moq/hang`,
`@moq/net`) to other esm.sh URLs, so it loads in the browser with no import map
or local build step:

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

For anything beyond embedding on a static page, install the package and use
a real bundler (the examples below).

## Web Component

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

**Attributes:**

- `url` (required): Relay server URL
- `name` (required): Broadcast name
- `controls`: Show playback controls (boolean)
- `paused`: Pause playback (boolean)
- `muted`: Mute audio (boolean)
- `volume`: Audio volume (0 to 1, default: 1)
- `reload`: Wait for (re)announcement before subscribing (default: true). Defaults off for `mediaoverquic.com` relays until they support broadcast discovery.
- `latency`: Latency target. `"real-time"` (default) derives it from RTT, or a number sets a fixed jitter buffer in ms. Collapses `latency-min` and `latency-max` to one value (minimize latency).
- `latency-min`: Latency floor (the jitter/startup buffer). Same units as `latency`; leaves the ceiling untouched.
- `latency-max`: Latency ceiling. `"real-time"` (default) minimizes latency; a number caps at that many ms. A ceiling above the floor enables [buffered playback](#buffered-playback): build up a buffer from future-dated frames instead of skipping ahead.
- `catalog-format`: Catalog format. One of `"hang"`, `"hangz"` (the [DEFLATE-compressed](/concept/layer/hang#compression) `catalog.json.z` track), `"msf"` (see [MSF](/concept/standard/msf)), or `"manual"` (supply the catalog yourself). When omitted, the format is auto-detected from the broadcast `name` extension (`.hang` or `.msf`), falling back to `"hang"`. `"hangz"` is opt-in only and never auto-detected (it shares the `.hang` suffix).

## Catalog Formats

`@moq/watch` can consume either the default [hang](/concept/layer/hang) catalog
or [MSF](/concept/standard/msf) (MoQ Streaming Format). The format is detected
from the broadcast name extension by default. `room/alice.hang` uses hang,
`room/alice.msf` uses MSF. Set `catalog-format` explicitly to override:

```html
<moq-watch
    url="https://relay.example.com/anon"
    name="room/alice.hang"
    catalog-format="msf">
    <canvas></canvas>
</moq-watch>
```

```typescript
import * as Watch from "@moq/watch";

const broadcast = new Watch.Broadcast({
    connection,
    enabled: true,
    name: "alice.hang",
    catalogFormat: "msf",
});

// or toggle at runtime
broadcast.catalogFormat.set("msf");
```

### Manual catalogs

Use `catalog-format="manual"` (or `catalogFormat: "manual"`) to skip the catalog
track entirely and supply a `Catalog.Root` directly. The connection and
broadcast name are still required, since they're used to subscribe to the media
tracks named by the catalog. Update the catalog at any time by writing to
the signal:

```typescript
import * as Watch from "@moq/watch";

const broadcast = new Watch.Broadcast({
    connection,
    enabled: true,
    name: "alice.hang",
    catalogFormat: "manual",
    catalog: {
        video: { renditions: { hd: { codec: "vp09.00.10.08", container: { kind: "legacy" } } } },
    },
});

// Replace at runtime
broadcast.catalog.set(nextCatalog);
```

The web component exposes the same field as a JS property:

```typescript
const el = document.querySelector("moq-watch")!;
el.catalogFormat = "manual";
el.catalog = myCatalog;
```

> Switching `catalogFormat` between `"manual"` and a fetched format (`"hang"` /
> `"msf"`) tears down the previous fetch loop, which clears `catalog`. Set the
> catalog *after* switching to `"manual"`, not before.

## Buffered playback

Latency is a **range**, `[latency-min, latency-max]`. By default the range is
collapsed (`latency` sets both to one value) and `@moq/watch` minimizes latency:
it anchors playback to the earliest frame seen relative to its timestamp and skips
ahead whenever the buffer grows past the target. That is right for live
conferencing, but wrong for content written *faster than real-time* with
timestamps in the future, such as a text-to-speech response streamed all at once.

Open the range, by setting a `latency-max` above the floor, to anchor playback to
the first frame received and play through at the encoded pace. The buffer is
allowed to float anywhere between the floor and the ceiling without skipping:

```html
<moq-watch url="https://relay.example.com/anon" name="bot/tts.hang"
    latency-min="100" latency-max="30000">
    <canvas></canvas>
</moq-watch>
```

In JavaScript, `latency` takes either a scalar (collapsed, minimize) or a range
object. The `latencyMin` / `latencyMax` properties are read-modify-write sugar
over the same `latency` value:

```typescript
const el = document.querySelector("moq-watch")!;
el.latency = { min: 100, max: 30_000 }; // floor 100ms, ceiling 30s
// equivalently, set the bounds independently:
el.latencyMin = 100;     // floor: start after 100ms buffered
el.latencyMax = 30_000;  // ceiling: never skip until 30s buffered
```

`latency-min` is the jitter/startup buffer (it can also be `"real-time"` for an
adaptive floor). `latency-max` is the ceiling, and it has two forms:

- a **number** (ms): buffer freely up to the cap, then skip ahead, so latency
  stays at most that far behind the newest frame.
- **`"real-time"`** (the default) or any value `<= latency-min`: collapsed, i.e.
  today's minimize-latency behavior.

The ceiling is always finite: the buffer is bounded by `latency-max` rather than
growing without limit. The mechanism is the same in every case: the playhead is
anchored on the first frame and only re-anchored (skipped forward) when keeping it
would push latency past `latency-max`. Minimize is just the degenerate case where
the ceiling equals the floor.

The buffered lookahead is held cheaply. The decoded audio ring only holds the
**floor** (`latency-min`) worth of PCM; everything above it stays upstream as
encoded frames, and the decoder applies backpressure (stops decoding ahead) until
the playhead nears each frame. So a large `latency-max` costs encoded bytes, not
seconds of decoded PCM.

At each new utterance, call `reset()` to flush the audio buffer and re-anchor
playback to the next frame. The producer can interrupt by writing a new utterance
(optionally on a new track) and the viewer calls `reset()` to drop whatever was
still buffered:

```typescript
el.reset();
```

This removes the need to pace writes on the producer: emit the whole response as
fast as possible with correct (future) timestamps, and `reset()` on interruption.

> Buffered playback uses the WebCodecs path (a `<canvas>` child). It does not
> apply to the MSE `<video>` path. Set the range before the broadcast starts
> decoding; changing it mid-stream takes effect on the next decoder.

### Custom tracks and catalog sections

A broadcast can carry arbitrary application tracks (for example a `meta.json`
metadata track) alongside the media. An application advertises them in its own
catalog section (the [catalog root](/concept/layer/hang#extensions) is a loose
object, so unknown sections pass through to `broadcast.catalog`).
`subscribeTrack(name, priority, consume)` follows the active broadcast across
reconnects and runs `consume(track, effect)` each time it becomes active. Decode
the payload yourself with the re-exported `@moq/json`:

```typescript
import { Json } from "@moq/watch";
import { Signals } from "@moq/watch";

// The app's own catalog section, read back from the loose catalog.
const section = broadcast.catalog.peek()?.scte35;

const scte35 = new Signals.Signal<{ splices: number[] } | undefined>(undefined);
broadcast.subscribeTrack("scte35.json", 100, (track, effect) => {
    const consumer = new Json.Consumer<{ splices: number[] }>(track);
    effect.spawn(async () => {
        for (;;) {
            const next = await Promise.race([effect.cancel, consumer.next()]);
            if (next === undefined) break;
            scte35.set(next);
        }
    });
});
```

The component exposes everything via its `broadcast` property
(`el.broadcast.subscribeTrack(...)`).

## UI Overlay

Import `@moq/watch/ui` for a Web Component overlay with buffering indicator, stats panel, and playback controls:

```html
<script type="module">
    import "@moq/watch/element";
    import "@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch
        url="https://relay.example.com/anon"
        name="room/alice.hang">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

The `<moq-watch-ui>` element automatically discovers the nested `<moq-watch>` and wires up reactive controls.

## JavaScript API

```typescript
import * as Watch from "@moq/watch";

const broadcast = new Watch.Broadcast({
    connection,
    enabled: true,
    name: "alice.hang",
});

// Disable the announcement gate for relays without broadcast discovery.
broadcast.reload.set(false);
```

## Related Packages

- **[@moq/publish](/lib/js/@moq/publish)**: Publish media to MoQ broadcasts
- **[@moq/hang](/lib/js/@moq/hang/)**: Core media library (catalog, container, support)
- **[@moq/net](/lib/js/@moq/net)**: Core pub/sub transport protocol

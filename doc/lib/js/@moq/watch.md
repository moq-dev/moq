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
- `catalog-format`: Catalog format. One of `"hang"`, `"msf"` (see [MSF](/concept/standard/msf)), or `"manual"` (supply the catalog yourself). When omitted, the format is auto-detected from the broadcast `name` extension (`.hang` or `.msf`), falling back to `"hang"`.

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
    reload: true,
});
```

## Related Packages

- **[@moq/publish](/lib/js/@moq/publish)**: Publish media to MoQ broadcasts
- **[@moq/hang](/lib/js/@moq/hang/)**: Core media library (catalog, container, support)
- **[@moq/net](/lib/js/@moq/net)**: Core pub/sub transport protocol

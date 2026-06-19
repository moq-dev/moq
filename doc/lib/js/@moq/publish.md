---
title: "@moq/publish"
description: Publish media to MoQ broadcasts
---

# @moq/publish

[![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Publish media to MoQ broadcasts. Provides both a JavaScript API and a `<moq-publish>` Web Component, plus an optional `<moq-publish-ui>` overlay.

## Installation

```bash
bun add @moq/publish
# or
npm add @moq/publish
```

### No-build CDN usage

For quick demos or single-page embeds where a bundler is overkill, load the
package straight from [esm.sh](https://esm.sh). esm.sh serves the package as a
browser-ready ESM module and rewrites bare imports (like `@moq/hang`,
`@moq/net`) to other esm.sh URLs, so it loads in the browser with no import map
or local build step:

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

For anything beyond embedding on a static page, install the package and use
a real bundler (the examples below).

## Web Component

```html
<script type="module">
    import "@moq/publish/element";
</script>

<moq-publish
    url="https://relay.example.com/anon"
    name="room/alice.hang"
    source="camera" simulcast>
    <video muted autoplay></video>
</moq-publish>
```

**Attributes:**

- `url` (required) - Relay server URL
- `name` (required) - Broadcast name
- `source` - Input to capture: `"camera"`, `"screen"`, or `"file"`
- `muted` - Mute audio capture (boolean)
- `invisible` - Disable video capture (boolean)
- `simulcast` - Also publish a lower-resolution `video/sd` rendition (a fraction of the source resolution) alongside `video/hd` (boolean)
- `preview` - What the preview renders: `"source"` (default), `"encoded"`, or `"none"` to disable it (see [Preview element](#preview-element))
- `announce` - When to publish the broadcast: `"source"` (default, hold off until a `source` is selected), `"always"` (announce as soon as the element connects), or `"never"`. The JS property takes the same string values (`el.announce = "always"`)

## Preview element

`<moq-publish>` discovers a nested `<video>` or `<canvas>` and uses it for a local preview.

- `<video>` attaches the raw capture stream via `srcObject`. This is the cheapest preview and the default.
- `<canvas>` draws the frames itself. With `preview="source"` (default) it draws the raw capture; with `preview="encoded"` it draws a decoded copy of the encoded video, so the preview shows exactly what a viewer receives over the network, codec artifacts and all.

The `encoded` mode costs a full extra encode + decode pass (it re-encodes with the same settings as the published rendition), so reach for it when you want to monitor the transmitted quality, not as the default preview. Set `preview="none"` to disable the preview without removing the element.

```html
<moq-publish url="https://relay.example.com/anon" name="room/alice.hang" source="camera" preview="encoded">
    <canvas></canvas>
</moq-publish>
```

## UI Overlay

Import `@moq/publish/ui` for a Web Component overlay with device selection and publishing controls:

```html
<script type="module">
    import "@moq/publish/element";
    import "@moq/publish/ui";
</script>

<moq-publish-ui>
    <moq-publish
        url="https://relay.example.com/anon"
        name="room/alice.hang"
        source="camera"
        simulcast>
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

The `<moq-publish-ui>` element automatically discovers the nested `<moq-publish>` and wires up reactive controls.
The overlay has no `simulcast` control; enable it via the attribute on the nested `<moq-publish>` as shown.

## JavaScript API

```typescript
import * as Publish from "@moq/publish";

const broadcast = new Publish.Broadcast({
    connection,
    enabled: true,
    name: "alice.hang",
    // Publish two video renditions: video/hd plus a lower-resolution video/sd.
    video: { hd: { enabled: true }, sd: { enabled: true } },
    audio: { enabled: true },
});

// Reactive controls
broadcast.name.set("bob.hang");
```

### Custom tracks and catalog sections

Beyond audio and video, you can publish arbitrary application tracks within the
same broadcast (no separate broadcast needed). `publishTrack(name, serve)` runs
`serve(track, effect)` for each subscriber; it rejects the built-in track names
(catalog/audio/video). Encode the payload yourself with the re-exported
`@moq/json`: a track-less `Json.Producer` is the same fan-out producer the catalog
uses, seeding late joiners with the latest value.

`publishTrack` does not touch the catalog; advertise the track by writing your own
section to `broadcast.catalog` (the [catalog root](/concept/layer/hang#extensions)
is a loose object, so any key passes through). This lets an app support something
like an `scte35` section with no hang-specific support:

```typescript
import { Json } from "@moq/publish";

const scte35 = new Json.Producer<{ splices: number[] }>({ initial: { splices: [] } });
broadcast.publishTrack("scte35.json", (track, effect) => scte35.serve(track, effect));
broadcast.catalog.mutate((c) => {
    c.scte35 = { track: "scte35.json" };
});
scte35.update({ splices: [42] });
```

The component exposes everything via its `broadcast` property
(`el.broadcast.publishTrack(...)`).

## Related Packages

- **[@moq/watch](/lib/js/@moq/watch)** — Subscribe to and render MoQ broadcasts
- **[@moq/hang](/lib/js/@moq/hang/)** — Core media library (catalog, container, support)
- **[@moq/net](/lib/js/@moq/net)** — Core pub/sub transport protocol

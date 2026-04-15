---
title: "@moq/publish"
description: Publish media to MoQ broadcasts
---

# @moq/publish

[![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Publish media to MoQ broadcasts. Provides both a JavaScript API and a `<moq-publish>` Web Component, plus an optional `<moq-publish-ui>` SolidJS overlay.

## Installation

```bash
bun add @moq/publish
# or
npm add @moq/publish
```

### No-build CDN usage

For quick demos or single-page embeds where a bundler is overkill, load the
package straight from jsDelivr with the `+esm` endpoint. jsDelivr transforms
the published file and rewrites bare imports (like `@moq/hang`, `@moq/lite`)
to other `+esm` URLs, so it loads in the browser with no import map or local
build step:

```html
<script type="module">
    import "https://cdn.jsdelivr.net/npm/@moq/publish/element.js/+esm";
    import "https://cdn.jsdelivr.net/npm/@moq/publish/ui/index.js/+esm";
</script>

<moq-publish-ui>
    <moq-publish url="https://relay.example.com/anon" name="room/alice" source="camera">
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

Pin a version range in the URL for production — e.g.
`https://cdn.jsdelivr.net/npm/@moq/publish@0.2/element.js/+esm`. [esm.sh](https://esm.sh)
(`https://esm.sh/@moq/publish/element`) works the same way if you prefer it.

For anything beyond embedding on a static page, install the package and use
a real bundler (the examples below).

## Web Component

```html
<script type="module">
    import "@moq/publish/element";
</script>

<moq-publish
    url="https://relay.example.com/anon"
    name="room/alice"
    audio video controls>
    <video muted autoplay></video>
</moq-publish>
```

**Attributes:**

- `url` (required) — Relay server URL
- `name` (required) — Broadcast name
- `device` — "camera" or "screen" (default: "camera")
- `audio` — Enable audio capture (boolean)
- `video` — Enable video capture (boolean)
- `controls` — Show publishing controls (boolean)

## UI Overlay

Import `@moq/publish/ui` for a SolidJS-powered overlay with device selection and publishing controls:

```html
<script type="module">
    import "@moq/publish/element";
    import "@moq/publish/ui";
</script>

<moq-publish-ui>
    <moq-publish
        url="https://relay.example.com/anon"
        name="room/alice"
        audio video>
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

The `<moq-publish-ui>` element automatically discovers the nested `<moq-publish>` and wires up reactive controls.

## JavaScript API

```typescript
import * as Publish from "@moq/publish";

const broadcast = new Publish.Broadcast({
    connection,
    enabled: true,
    name: "alice",
    video: { enabled: true, device: "camera" },
    audio: { enabled: true },
});

// Reactive controls
broadcast.video.device.set("screen");
broadcast.name.set("bob");
```

## Related Packages

- **[@moq/watch](/js/@moq/watch)** — Subscribe to and render MoQ broadcasts
- **[@moq/hang](/js/@moq/hang/)** — Core media library (catalog, container, support)
- **[@moq/ui-core](/js/@moq/ui-core)** — Shared UI primitives
- **[@moq/lite](/js/@moq/lite)** — Core pub/sub transport protocol

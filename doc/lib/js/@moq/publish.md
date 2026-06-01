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
    name: "alice.hang",
    video: { enabled: true, device: "camera" },
    audio: { enabled: true },
});

// Reactive controls
broadcast.video.device.set("screen");
broadcast.name.set("bob.hang");
```

## Related Packages

- **[@moq/watch](/lib/js/@moq/watch)** — Subscribe to and render MoQ broadcasts
- **[@moq/hang](/lib/js/@moq/hang/)** — Core media library (catalog, container, support)
- **[@moq/net](/lib/js/@moq/net)** — Core pub/sub transport protocol

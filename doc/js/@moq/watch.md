---
title: "@moq/watch"
description: Subscribe to and render MoQ broadcasts
---

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Subscribe to and render MoQ broadcasts. Provides both a JavaScript API and a `<moq-watch>` Web Component, plus an optional `<moq-watch-ui>` SolidJS overlay.

## Installation

```bash
bun add @moq/watch
# or
npm add @moq/watch
```

### No-build CDN usage

For quick demos or single-page embeds where a bundler is overkill, load the
package straight from jsDelivr with the `+esm` endpoint. jsDelivr transforms
the published file and rewrites bare imports (like `@moq/hang`, `@moq/lite`)
to other `+esm` URLs, so it loads in the browser with no import map or local
build step:

```html
<script type="module">
    import "https://cdn.jsdelivr.net/npm/@moq/watch/element.js/+esm";
    import "https://cdn.jsdelivr.net/npm/@moq/watch/ui/index.js/+esm";
</script>

<moq-watch-ui>
    <moq-watch url="https://relay.example.com/anon" name="room/alice">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

Pin a version range in the URL for production — e.g.
`https://cdn.jsdelivr.net/npm/@moq/watch@0.2/element.js/+esm`. [esm.sh](https://esm.sh)
(`https://esm.sh/@moq/watch/element`) works the same way if you prefer it.

For anything beyond embedding on a static page, install the package and use
a real bundler (the examples below).

## Web Component

```html
<script type="module">
    import "@moq/watch/element";
</script>

<moq-watch
    url="https://relay.example.com/anon"
    name="room/alice"
    controls>
    <canvas></canvas>
</moq-watch>
```

**Attributes:**

- `url` (required) — Relay server URL
- `name` (required) — Broadcast name
- `controls` — Show playback controls (boolean)
- `paused` — Pause playback (boolean)
- `muted` — Mute audio (boolean)
- `volume` — Audio volume (0–1, default: 1)
- `catalog-format` — Catalog format: `"hang"` (default) or `"msf"` (see [MSF](/concept/standard/msf))

## Catalog Formats

`@moq/watch` can consume either the default [hang](/concept/layer/hang) catalog
or [MSF](/concept/standard/msf) (MoQ Streaming Format). Set `catalog-format="msf"`
on the element, or assign the `catalogFormat` signal on the `Broadcast` to
switch formats at runtime:

```html
<moq-watch
    url="https://relay.example.com/anon"
    name="room/alice"
    catalog-format="msf">
    <canvas></canvas>
</moq-watch>
```

```typescript
import * as Watch from "@moq/watch";

const broadcast = new Watch.Broadcast({
    connection,
    enabled: true,
    name: "alice",
    catalogFormat: "msf",
});

// or toggle at runtime
broadcast.catalogFormat.set("msf");
```

## UI Overlay

Import `@moq/watch/ui` for a SolidJS-powered overlay with buffering indicator, stats panel, and playback controls:

```html
<script type="module">
    import "@moq/watch/element";
    import "@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch
        url="https://relay.example.com/anon"
        name="room/alice">
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
    name: "alice",
    reload: true,
});
```

## Related Packages

- **[@moq/publish](/js/@moq/publish)** — Publish media to MoQ broadcasts
- **[@moq/hang](/js/@moq/hang/)** — Core media library (catalog, container, support)
- **[@moq/ui-core](/js/@moq/ui-core)** — Shared UI primitives
- **[@moq/lite](/js/@moq/lite)** — Core pub/sub transport protocol

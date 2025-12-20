---
title: Web Components
description: Web Components API reference
---

# Web Components

`@moq/hang` provides Web Components for easy integration into any web page or framework.

## Why Web Components?

- **Framework agnostic** - Works with React, Vue, Solid, or vanilla JS
- **Easy integration** - Just import and use like HTML
- **Encapsulated** - Shadow DOM for style isolation
- **Reactive** - Automatically update when attributes change

## Available Components

### `<hang-publish>`

Publish camera/microphone or screen as a MoQ broadcast.

**Attributes:**
- `url` (required) - Relay server URL
- `path` (required) - Broadcast path/name
- `device` - "camera" or "screen" (default: "camera")
- `audio` - Enable audio capture (boolean)
- `video` - Enable video capture (boolean)
- `controls` - Show publishing controls (boolean)

**Example:**

```html
<script type="module">
    import "@moq/hang/publish/element";
</script>

<hang-publish
    url="https://relay.example.com/anon"
    path="room/alice"
    device="camera"
    audio video controls>
    <!-- Optional preview element -->
    <video muted autoplay style="width: 100%"></video>
</hang-publish>
```

### `<hang-watch>`

Subscribe to and render a MoQ broadcast.

**Attributes:**
- `url` (required) - Relay server URL
- `path` (required) - Broadcast path/name
- `controls` - Show playback controls (boolean)
- `paused` - Pause playback (boolean)
- `muted` - Mute audio (boolean)
- `volume` - Audio volume (0-1, default: 1)

**Example:**

```html
<script type="module">
    import "@moq/hang/watch/element";
</script>

<hang-watch
    url="https://relay.example.com/anon"
    path="room/alice"
    volume="0.8"
    controls>
    <!-- Optional canvas for video rendering -->
    <canvas style="width: 100%"></canvas>
</hang-watch>
```

### `<hang-meet>`

Video conferencing component that discovers and renders multiple broadcasts.

**Attributes:**
- `url` (required) - Relay server URL
- `path` (required) - Room path prefix
- `audio` - Enable audio (boolean)
- `video` - Enable video (boolean)
- `controls` - Show controls (boolean)

**Example:**

```html
<script type="module">
    import "@moq/hang/meet/element";
</script>

<hang-meet
    url="https://relay.example.com/anon"
    path="room123"
    audio video controls>
    <!-- Optional: add local publish component -->
    <hang-publish path="room123/me" audio video></hang-publish>
</hang-meet>
```

This discovers any broadcasts starting with `room123/` and renders them in a grid.

### `<hang-support>`

Display browser support information.

**Attributes:**
- `mode` - "publish" or "watch"
- `show` - "always", "partial", or "never" (default: "partial")

**Example:**

```html
<script type="module">
    import "@moq/hang/support/element";
</script>

<!-- Show only when publishing is not supported -->
<hang-support mode="publish" show="partial"></hang-support>
```

## Using JavaScript Properties

HTML attributes are strings, but JavaScript properties are typed and reactive:

```typescript
// Get element reference
const watch = document.querySelector("hang-watch") as HangWatch;

// Set properties (reactive)
watch.volume.set(0.8);
watch.muted.set(false);
watch.paused.set(true);

// Subscribe to changes
watch.volume.subscribe((vol) => {
    console.log("Volume changed:", vol);
});

// Get current value
const currentVolume = watch.volume.get();
```

## Reactive Properties

All properties are signals from `@moq/signals`:

```typescript
import { HangWatch } from "@moq/hang/watch/element";

const watch = document.querySelector("hang-watch") as HangWatch;

// These are all reactive signals:
watch.volume    // Signal<number>
watch.muted     // Signal<boolean>
watch.paused    // Signal<boolean>
watch.url       // Signal<string>
watch.path      // Signal<string>
```

## Framework Integration

### React

```tsx
import { useEffect, useRef } from "react";
import "@moq/hang/watch/element";

function VideoPlayer({ url, path }) {
    const ref = useRef<HangWatch>(null);

    useEffect(() => {
        if (ref.current) {
            ref.current.volume.set(0.8);
        }
    }, []);

    return (
        <hang-watch
            ref={ref}
            url={url}
            path={path}
            controls>
            <canvas />
        </hang-watch>
    );
}
```

### SolidJS

Use `@moq/hang-ui` for native SolidJS components, or use Web Components directly:

```tsx
import "@moq/hang/watch/element";

function VideoPlayer(props) {
    return (
        <hang-watch
            url={props.url}
            path={props.path}
            controls>
            <canvas />
        </hang-watch>
    );
}
```

### Vue

```vue
<template>
    <hang-watch
        :url="url"
        :path="path"
        controls>
        <canvas />
    </hang-watch>
</template>

<script>
import "@moq/hang/watch/element";

export default {
    props: ["url", "path"],
};
</script>
```

## Styling

Web Components use Shadow DOM, so global styles won't apply. Use CSS custom properties (variables) or style child elements:

```html
<style>
hang-watch::part(video) {
    border-radius: 8px;
}

hang-watch canvas {
    width: 100%;
    border-radius: 8px;
}
</style>

<hang-watch url="..." path="..." controls>
    <canvas style="width: 100%; border-radius: 8px;"></canvas>
</hang-watch>
```

## Tree-Shaking

To prevent tree-shaking from removing component registrations, explicitly import with `/element` suffix:

```typescript
// Correct
import "@moq/hang/watch/element";

// May be tree-shaken (don't use)
import "@moq/hang";
```

## TypeScript Support

Full TypeScript support with type definitions:

```typescript
import type { HangWatch, HangPublish, HangMeet } from "@moq/hang";

const watch: HangWatch = document.querySelector("hang-watch")!;
const publish: HangPublish = document.querySelector("hang-publish")!;
```

## Events

Components emit custom events:

```typescript
const watch = document.querySelector("hang-watch") as HangWatch;

watch.addEventListener("play", () => {
    console.log("Playback started");
});

watch.addEventListener("pause", () => {
    console.log("Playback paused");
});

watch.addEventListener("error", (e) => {
    console.error("Error:", e.detail);
});
```

## Examples

See the [hang-demo](https://github.com/moq-dev/moq/tree/main/js/hang-demo) for complete examples:

- Basic video player
- Video conferencing
- Screen sharing
- Chat integration

[View more examples →](/typescript/examples)

## Next Steps

- Try the [JavaScript API](/typescript/hang)
- View [@moq/hang-ui](https://www.npmjs.com/package/@moq/hang-ui) for SolidJS components
- Read [code examples](/typescript/examples)
- Learn about [@moq/lite](/typescript/lite) for custom protocols

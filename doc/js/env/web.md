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

### `<moq-publish>`

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
    import "@moq/publish/element";
</script>

<moq-publish
    url="https://relay.example.com/anon"
    path="room/alice"
    device="camera"
    audio video controls>
    <!-- Optional preview element -->
    <video muted autoplay style="width: 100%"></video>
</moq-publish>
```

### `<moq-watch>`

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
    import "@moq/watch/element";
</script>

<moq-watch
    url="https://relay.example.com/anon"
    path="room/alice"
    volume="0.8"
    controls>
    <!-- Optional canvas for video rendering -->
    <canvas style="width: 100%"></canvas>
</moq-watch>
```

### `<moq-watch-support>`

Display browser support information for watching streams.

**Attributes:**
- `show` - "always", "warning", "error", or "never" (default: "warning")
- `details` - show detailed codec information

**Example:**

```html
<script type="module">
    import "@moq/watch/support/element";
</script>

<!-- Show only when a polyfill/fallback is needed -->
<moq-watch-support show="warning"></moq-watch-support>
```

### `<moq-publish-support>`

Display browser support information for publishing streams.

**Attributes:**
- `show` - "always", "warning", "error", or "never" (default: "warning")
- `details` - show detailed codec information

**Example:**

```html
<script type="module">
    import "@moq/publish/support/element";
</script>

<!-- Show only when a polyfill/fallback is needed -->
<moq-publish-support show="warning"></moq-publish-support>
```

## Using JavaScript Properties

HTML attributes are strings, but JavaScript properties are typed and reactive:

```typescript
// Get element reference
const watch = document.querySelector("moq-watch") as MoqWatch;

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
import MoqWatch from "@moq/watch/element";

const watch = document.querySelector("moq-watch") as MoqWatch;

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
import "@moq/watch/element";

function VideoPlayer({ url, path }) {
    const ref = useRef<MoqWatch>(null);

    useEffect(() => {
        if (ref.current) {
            ref.current.volume.set(0.8);
        }
    }, []);

    return (
        <moq-watch
            ref={ref}
            url={url}
            path={path}
            controls>
            <canvas />
        </moq-watch>
    );
}
```

### SolidJS

Use `@moq/watch/ui` and `@moq/publish/ui` for SolidJS UI overlays, or use Web Components directly:

```tsx
import "@moq/watch/element";

function VideoPlayer(props) {
    return (
        <moq-watch
            url={props.url}
            path={props.path}
            controls>
            <canvas />
        </moq-watch>
    );
}
```

### Vue

```vue
<template>
    <moq-watch
        :url="url"
        :path="path"
        controls>
        <canvas />
    </moq-watch>
</template>

<script>
import "@moq/watch/element";

export default {
    props: ["url", "path"],
};
</script>
```

## Styling

Web Components use Shadow DOM, so global styles won't apply. Use CSS custom properties (variables) or style child elements:

```html
<style>
moq-watch::part(video) {
    border-radius: 8px;
}

moq-watch canvas {
    width: 100%;
    border-radius: 8px;
}
</style>

<moq-watch url="..." path="..." controls>
    <canvas style="width: 100%; border-radius: 8px;"></canvas>
</moq-watch>
```

## Tree-Shaking

To prevent tree-shaking from removing component registrations, explicitly import with `/element` suffix:

```typescript
// Correct
import "@moq/watch/element";

// May be tree-shaken (don't use)
import "@moq/watch";
```

## TypeScript Support

Full TypeScript support with type definitions:

```typescript
import MoqWatch from "@moq/watch/element";
import MoqPublish from "@moq/publish/element";

const watch: MoqWatch = document.querySelector("moq-watch")!;
const publish: MoqPublish = document.querySelector("moq-publish")!;
```

## Events

Components emit custom events:

```typescript
const watch = document.querySelector("moq-watch") as MoqWatch;

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

## Browser Compatibility

Requires modern browser features:

- **WebTransport** - Chromium-based browsers (Chrome, Edge, Brave)
- **WebCodecs** - For media encoding/decoding
- **WebAudio** - For audio playback

**Supported browsers:**
- Chrome 97+
- Edge 97+
- Brave (recent versions)

**Experimental support:**
- Firefox (behind flag)
- Safari (future support planned)

## Production Deployment

For production, you'll want to:

1. Use a production relay ([moq-relay](/app/relay/))
2. Set up proper [authentication](/app/relay/auth)
3. Use a bundler like [Vite](https://vite.dev/)

Currently, you need to use a bundler and [Vite](https://vite.dev/) is the only supported option for `@moq/watch` and `@moq/publish`.
It makes me very sad and we're working on a more universal solution, contributions welcome!

**NOTE** both of these libraries are intended for client-side.
However, `@moq/lite` can run on the server side using [Deno](https://deno.com/) or a [WebTransport polyfill](https://github.com/moq-dev/web-transport/tree/main/web-transport-ws).
Don't even try to run `@moq/hang` on the server side or you'll run into a ton of issues, *especially* with Next.js.

## Next Steps

- Learn about [@moq/hang](/js/@moq/hang/)
- Use [@moq/lite](/js/@moq/lite) for custom protocols
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)

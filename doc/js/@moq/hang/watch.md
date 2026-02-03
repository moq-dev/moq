---
title: Watching Streams
description: Subscribe to and render MoQ broadcasts
---

# Watching Streams

This guide covers how to subscribe to and render MoQ broadcasts using `@moq/hang`.

## Web Component

The simplest way to watch a stream:

```html
<script type="module">
    import "@moq/hang/watch/element";
</script>

<hang-watch
    url="https://relay.example.com/anon"
    path="room/alice"
    controls>
    <canvas></canvas>
</hang-watch>
```

### Attributes

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| `url` | string | required | Relay server URL |
| `path` | string | required | Broadcast path |
| `controls` | boolean | false | Show playback controls |
| `paused` | boolean | false | Pause playback |
| `muted` | boolean | false | Mute audio |
| `volume` | number | 1 | Audio volume (0-1) |

### Events

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

## JavaScript API

For more control:

```typescript
import * as Hang from "@moq/hang";

const connection = new Hang.Connection("https://relay.example.com/anon");

const watch = new Hang.Watch.Broadcast(connection, {
    enabled: true,
    name: "alice",
    video: { enabled: true },
    audio: { enabled: true },
});

// Access the video stream
watch.video.media.subscribe((stream) => {
    if (stream) {
        videoElement.srcObject = stream;
    }
});

// Access audio
watch.audio.media.subscribe((stream) => {
    if (stream) {
        audioElement.srcObject = stream;
    }
});
```

## Playback Controls

### Pause/Resume

```typescript
// Using attribute
watch.setAttribute("paused", "");
watch.removeAttribute("paused");

// Using JavaScript property
watch.paused.set(true);
watch.paused.set(false);
```

### Volume Control

```typescript
// Set volume (0-1)
watch.volume.set(0.5);

// Mute/unmute
watch.muted.set(true);
watch.muted.set(false);
```

### Playback State

```typescript
// Subscribe to state changes
watch.paused.subscribe((isPaused) => {
    console.log("Paused:", isPaused);
});

watch.volume.subscribe((vol) => {
    console.log("Volume:", vol);
});
```

## Track Selection

When a broadcast has multiple tracks (e.g., multiple quality levels):

```typescript
const watch = new Hang.Watch.Broadcast(connection, {
    name: "stream",
    video: {
        enabled: true,
        // Optionally specify track name
        track: "video-720p",
    },
});

// Switch tracks
watch.video.track.set("video-1080p");
```

## Quality Selection

Access track information from the catalog:

```typescript
// Subscribe to catalog updates
watch.catalog.subscribe((catalog) => {
    if (catalog) {
        console.log("Available tracks:", catalog.tracks);

        // Find video tracks
        const videoTracks = catalog.tracks.filter(t => t.kind === "video");
        console.log("Video qualities:", videoTracks.map(t => ({
            name: t.name,
            width: t.width,
            height: t.height,
            bitrate: t.bitrate,
        })));
    }
});
```

## Buffering and Latency

Monitor connection status:

```typescript
// Check connection state
watch.connection.state.subscribe((state) => {
    console.log("Connection state:", state);
});

// Monitor for buffering
watch.video.buffering.subscribe((isBuffering) => {
    if (isBuffering) {
        showLoadingSpinner();
    } else {
        hideLoadingSpinner();
    }
});
```

## Error Handling

```typescript
watch.error.subscribe((error) => {
    if (error) {
        console.error("Watch error:", error);

        // Handle specific errors
        if (error.code === "NOT_FOUND") {
            showMessage("Stream not found");
        } else if (error.code === "PERMISSION_DENIED") {
            showMessage("Access denied");
        }
    }
});
```

## React Integration

```tsx
import { useEffect, useRef } from "react";
import "@moq/hang/watch/element";
import type { HangWatch } from "@moq/hang";

function VideoPlayer({ url, path }) {
    const watchRef = useRef<HangWatch>(null);

    useEffect(() => {
        const watch = watchRef.current;
        if (!watch) return;

        // Subscribe to events
        const handleError = (e) => console.error(e.detail);
        watch.addEventListener("error", handleError);

        return () => {
            watch.removeEventListener("error", handleError);
        };
    }, []);

    return (
        <hang-watch
            ref={watchRef}
            url={url}
            path={path}
            controls>
            <canvas style={{ width: "100%" }} />
        </hang-watch>
    );
}
```

## SolidJS Integration

Use `@moq/hang-ui` for native components:

```tsx
import { HangWatch } from "@moq/hang-ui/watch";

function VideoPlayer(props) {
    return (
        <HangWatch
            url={props.url}
            path={props.path}
            controls
        />
    );
}
```

Or use Web Components directly:

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

## Next Steps

- Learn about [publishing streams](/js/@moq/hang/publish)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Learn about [Web Components](/js/env/web)

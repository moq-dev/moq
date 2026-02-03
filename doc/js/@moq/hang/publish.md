---
title: Publishing Streams
description: Publish camera, microphone, or screen to MoQ
---

# Publishing Streams

This guide covers how to publish media to MoQ relays using `@moq/hang`.

## Web Component

The simplest way to publish:

```html
<script type="module">
    import "@moq/hang/publish/element";
</script>

<hang-publish
    url="https://relay.example.com/anon"
    path="room/alice"
    device="camera"
    audio video controls>
    <video muted autoplay></video>
</hang-publish>
```

### Attributes

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| `url` | string | required | Relay server URL |
| `path` | string | required | Broadcast path |
| `device` | string | "camera" | "camera" or "screen" |
| `audio` | boolean | false | Enable audio |
| `video` | boolean | false | Enable video |
| `controls` | boolean | false | Show controls |

### Events

```typescript
const publish = document.querySelector("hang-publish") as HangPublish;

publish.addEventListener("start", () => {
    console.log("Publishing started");
});

publish.addEventListener("stop", () => {
    console.log("Publishing stopped");
});

publish.addEventListener("error", (e) => {
    console.error("Error:", e.detail);
});
```

## JavaScript API

For more control:

```typescript
import * as Hang from "@moq/hang";

const connection = new Hang.Connection("https://relay.example.com/anon");

const publish = new Hang.Publish.Broadcast(connection, {
    enabled: true,
    name: "alice",
    video: {
        enabled: true,
        device: "camera",
        bitrate: 2_500_000,
        framerate: 30,
    },
    audio: {
        enabled: true,
        bitrate: 128_000,
    },
});

// Access the local preview
publish.video.media.subscribe((stream) => {
    if (stream) {
        previewVideo.srcObject = stream;
    }
});
```

## Device Selection

### Camera

```typescript
const publish = new Hang.Publish.Broadcast(connection, {
    name: "stream",
    video: {
        enabled: true,
        device: "camera",
    },
});
```

### Screen Share

```typescript
const publish = new Hang.Publish.Broadcast(connection, {
    name: "stream",
    video: {
        enabled: true,
        device: "screen",
    },
});
```

### Switching Devices

```typescript
// Switch from camera to screen
publish.video.device.set("screen");

// Switch back to camera
publish.video.device.set("camera");
```

### Specific Camera Selection

```typescript
// Get available devices
const devices = await navigator.mediaDevices.enumerateDevices();
const cameras = devices.filter(d => d.kind === "videoinput");

// Select specific camera by deviceId
publish.video.deviceId.set(cameras[1].deviceId);
```

## Quality Settings

### Video Quality

```typescript
const publish = new Hang.Publish.Broadcast(connection, {
    name: "stream",
    video: {
        enabled: true,
        width: 1920,
        height: 1080,
        framerate: 30,
        bitrate: 5_000_000,
    },
});

// Change bitrate dynamically
publish.video.bitrate.set(2_500_000);

// Change framerate
publish.video.framerate.set(15);
```

### Audio Quality

```typescript
const publish = new Hang.Publish.Broadcast(connection, {
    name: "stream",
    audio: {
        enabled: true,
        bitrate: 128_000,
        sampleRate: 48000,
        channels: 2,
    },
});
```

### Codec Selection

```typescript
const publish = new Hang.Publish.Broadcast(connection, {
    name: "stream",
    video: {
        enabled: true,
        codec: "avc1.640028", // H.264 High Profile
        // codec: "hev1.1.6.L93.B0", // H.265
        // codec: "vp09.00.10.08", // VP9
        // codec: "av01.0.04M.08", // AV1
    },
    audio: {
        enabled: true,
        codec: "opus", // or "mp4a.40.2" for AAC
    },
});
```

## Enable/Disable Tracks

```typescript
// Disable video (audio only)
publish.video.enabled.set(false);

// Re-enable video
publish.video.enabled.set(true);

// Mute audio
publish.audio.enabled.set(false);
```

## Publishing State

```typescript
// Check if publishing
publish.enabled.subscribe((isEnabled) => {
    console.log("Publishing:", isEnabled);
});

// Start publishing
publish.enabled.set(true);

// Stop publishing
publish.enabled.set(false);
```

## Error Handling

```typescript
publish.error.subscribe((error) => {
    if (error) {
        console.error("Publish error:", error);

        if (error.code === "PERMISSION_DENIED") {
            showMessage("Camera/microphone access denied");
        } else if (error.code === "NOT_SUPPORTED") {
            showMessage("WebRTC not supported");
        }
    }
});
```

## Permission Handling

```typescript
// Request permissions before publishing
try {
    await navigator.mediaDevices.getUserMedia({ video: true, audio: true });
    // Permissions granted, safe to publish
    publish.enabled.set(true);
} catch (error) {
    if (error.name === "NotAllowedError") {
        showMessage("Please allow camera and microphone access");
    }
}
```

## React Integration

```tsx
import { useEffect, useRef, useState } from "react";
import "@moq/hang/publish/element";
import type { HangPublish } from "@moq/hang";

function Publisher({ url, path }) {
    const publishRef = useRef<HangPublish>(null);
    const [isPublishing, setIsPublishing] = useState(false);

    const togglePublish = () => {
        const publish = publishRef.current;
        if (publish) {
            publish.enabled.set(!isPublishing);
            setIsPublishing(!isPublishing);
        }
    };

    return (
        <div>
            <hang-publish
                ref={publishRef}
                url={url}
                path={path}
                audio video>
                <video muted autoplay style={{ width: "100%" }} />
            </hang-publish>

            <button onClick={togglePublish}>
                {isPublishing ? "Stop" : "Start"} Publishing
            </button>
        </div>
    );
}
```

## SolidJS Integration

Use `@moq/hang-ui`:

```tsx
import { HangPublish } from "@moq/hang-ui/publish";

function Publisher(props) {
    return (
        <HangPublish
            url={props.url}
            path={props.path}
            audio video controls
        />
    );
}
```

## Authentication

Include JWT token in the URL:

```html
<hang-publish
    url="https://relay.example.com/room/123?jwt=eyJhbGciOiJIUzI1NiIs..."
    path="alice"
    audio video>
</hang-publish>
```

See [Authentication](/app/relay/auth) for token generation.

## Next Steps

- Learn about [watching streams](/js/@moq/hang/watch)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Learn about [Web Components](/js/env/web)

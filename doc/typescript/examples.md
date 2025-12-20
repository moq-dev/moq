---
title: TypeScript Examples
description: Code examples for TypeScript libraries
---

# TypeScript Examples

Code examples for common use cases with the TypeScript libraries.

## Basic Examples

### Simple Video Player

```html
<!DOCTYPE html>
<html>
<head>
    <script type="module">
        import "@moq/hang/watch/element";
    </script>
</head>
<body>
    <hang-watch
        url="https://relay.example.com/anon"
        path="demo/stream"
        controls>
        <canvas style="width: 100%"></canvas>
    </hang-watch>
</body>
</html>
```

### Camera Publisher

```html
<!DOCTYPE html>
<html>
<head>
    <script type="module">
        import "@moq/hang/publish/element";
    </script>
</head>
<body>
    <hang-publish
        url="https://relay.example.com/anon"
        path="demo/my-camera"
        device="camera"
        audio video controls>
        <video muted autoplay></video>
    </hang-publish>
</body>
</html>
```

### Chat Application

```typescript
import * as Moq from "@moq/lite";

const connection = await Moq.connect("https://relay.example.com/anon");

// Publishing messages
const broadcast = new Moq.BroadcastProducer();
const track = broadcast.createTrack("chat");

function sendMessage(text: string) {
    const group = track.appendGroup();
    group.writeString(JSON.stringify({
        user: "Alice",
        message: text,
        timestamp: Date.now(),
    }));
    group.close();
}

connection.publish("room-123", broadcast.consume());

// Subscribing to messages
const chatBroadcast = connection.consume("room-123");
const chatTrack = await chatBroadcast.subscribe("chat");

for await (const group of chatTrack) {
    const data = await group.readString();
    if (data) {
        const message = JSON.parse(data);
        console.log(`${message.user}: ${message.message}`);
    }
}
```

## Advanced Examples

### Screen Sharing

```html
<!DOCTYPE html>
<html>
<head>
    <script type="module">
        import "@moq/hang/publish/element";
        import "@moq/hang/watch/element";
    </script>
</head>
<body>
    <h2>Share Screen</h2>
    <hang-publish
        url="https://relay.example.com/anon"
        path="screen-share/alice"
        device="screen"
        audio video controls>
    </hang-publish>

    <h2>View Screen Share</h2>
    <hang-watch
        url="https://relay.example.com/anon"
        path="screen-share/alice"
        controls>
        <canvas></canvas>
    </hang-watch>
</body>
</html>
```

### Video Conference

```html
<!DOCTYPE html>
<html>
<head>
    <script type="module">
        import "@moq/hang/meet/element";
    </script>
    <style>
        hang-meet {
            display: block;
            width: 100%;
            height: 100vh;
        }
    </style>
</head>
<body>
    <hang-meet
        url="https://relay.example.com/anon"
        path="conference/room-123"
        audio video controls>
        <!-- Publish your own camera -->
        <hang-publish
            path="conference/room-123/alice"
            audio video>
        </hang-publish>
    </hang-meet>
</body>
</html>
```

### Dynamic Controls

```html
<!DOCTYPE html>
<html>
<head>
    <script type="module">
        import "@moq/hang/watch/element";

        window.onload = () => {
            const watch = document.querySelector("hang-watch");

            document.getElementById("play").onclick = () => {
                watch.paused.set(false);
            };

            document.getElementById("pause").onclick = () => {
                watch.paused.set(true);
            };

            document.getElementById("mute").onclick = () => {
                watch.muted.set(true);
            };

            document.getElementById("unmute").onclick = () => {
                watch.muted.set(false);
            };

            document.getElementById("volume").oninput = (e) => {
                watch.volume.set(e.target.value / 100);
            };
        };
    </script>
</head>
<body>
    <hang-watch
        url="https://relay.example.com/anon"
        path="demo/stream">
        <canvas></canvas>
    </hang-watch>

    <div>
        <button id="play">Play</button>
        <button id="pause">Pause</button>
        <button id="mute">Mute</button>
        <button id="unmute">Unmute</button>
        <input id="volume" type="range" min="0" max="100" value="100">
    </div>
</body>
</html>
```

### React Integration

```tsx
import { useEffect, useRef, useState } from "react";
import "@moq/hang/watch/element";
import type { HangWatch } from "@moq/hang";

function VideoPlayer({ url, path }: { url: string; path: string }) {
    const watchRef = useRef<HangWatch>(null);
    const [volume, setVolume] = useState(1.0);
    const [muted, setMuted] = useState(false);

    useEffect(() => {
        if (watchRef.current) {
            watchRef.current.volume.set(volume);
        }
    }, [volume]);

    useEffect(() => {
        if (watchRef.current) {
            watchRef.current.muted.set(muted);
        }
    }, [muted]);

    return (
        <div>
            <hang-watch
                ref={watchRef}
                url={url}
                path={path}
                controls>
                <canvas style={{ width: "100%" }} />
            </hang-watch>

            <div>
                <button onClick={() => setMuted(!muted)}>
                    {muted ? "Unmute" : "Mute"}
                </button>
                <input
                    type="range"
                    min="0"
                    max="1"
                    step="0.1"
                    value={volume}
                    onChange={(e) => setVolume(Number(e.target.value))}
                />
            </div>
        </div>
    );
}
```

### SolidJS Integration

Use `@moq/hang-ui` for native SolidJS components:

```tsx
import { HangWatch } from "@moq/hang-ui";

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

### JavaScript API

```typescript
import * as Hang from "@moq/hang";

// Create connection
const connection = new Hang.Connection("https://relay.example.com/anon");

// Wait for connection
await connection.established;

// Publish media
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
    },
});

// Subscribe to media
const watch = new Hang.Watch.Broadcast(connection, {
    enabled: true,
    name: "bob",
    video: { enabled: true },
    audio: { enabled: true },
});

// React to state changes
watch.video.media.subscribe((stream) => {
    if (stream) {
        videoElement.srcObject = stream;
    }
});

// Control playback
watch.paused.set(true);
watch.volume.set(0.8);
```

### Custom Protocol

Build custom protocols on top of `@moq/lite`:

```typescript
import * as Moq from "@moq/lite";

class GameStatePublisher {
    private broadcast: Moq.BroadcastProducer;
    private track: Moq.Track;

    constructor(private connection: Moq.Connection) {
        this.broadcast = new Moq.BroadcastProducer();
        this.track = this.broadcast.createTrack("game-state");
        this.connection.publish("game-123", this.broadcast.consume());
    }

    updatePlayerPosition(playerId: string, x: number, y: number) {
        const group = this.track.appendGroup();
        group.writeString(JSON.stringify({
            type: "player-move",
            playerId,
            position: { x, y },
            timestamp: Date.now(),
        }));
        group.close();
    }

    sendChatMessage(playerId: string, message: string) {
        const group = this.track.appendGroup();
        group.writeString(JSON.stringify({
            type: "chat",
            playerId,
            message,
            timestamp: Date.now(),
        }));
        group.close();
    }
}
```

## Demo Application

Check out the complete demo application:

[hang-demo on GitHub](https://github.com/moq-dev/moq/tree/main/js/hang-demo)

Features:
- Video conferencing
- Screen sharing
- Text chat
- Quality selector
- Network statistics

## Next Steps

- Learn about [Web Components](/typescript/web-components)
- Read the [@moq/hang API](/typescript/hang)
- Read the [@moq/lite API](/typescript/lite)
- View [Rust examples](/rust/examples)

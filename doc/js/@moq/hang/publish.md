---
title: Publishing Streams
description: Publish camera, microphone, or screen to MoQ
---

# Publishing Streams

This guide covers how to publish media to MoQ relays using `@moq/publish`.

## Web Component

The simplest way to publish:

```html
<script type="module">
    import "@moq/publish/element";
</script>

<moq-publish
    url="https://relay.example.com/anon"
    name="room/alice"
    device="camera"
    audio video controls>
    <video muted autoplay></video>
</moq-publish>
```

### Attributes

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| `url` | string | required | Relay server URL |
| `name` | string | required | Broadcast name |
| `device` | string | "camera" | "camera" or "screen" |
| `audio` | boolean | false | Enable audio |
| `video` | boolean | false | Enable video |
| `controls` | boolean | false | Show controls |

## JavaScript API

For more control, use `@moq/publish` directly:

```typescript
import * as Moq from "@moq/lite";
import * as Publish from "@moq/publish";

const connection = await Moq.Connection.connect(
    new URL("https://relay.example.com/anon")
);

const publish = new Publish.Broadcast({
    connection,
    enabled: true,
    name: "alice",
    video: {
        enabled: true,
        device: "camera",
    },
    audio: {
        enabled: true,
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

### Enable/Disable Tracks

```typescript
// Disable video (audio only)
publish.video.enabled.set(false);

// Re-enable video
publish.video.enabled.set(true);

// Mute audio
publish.audio.enabled.set(false);
```

## SolidJS Integration

Use `@moq/publish/ui` for the SolidJS UI overlay. The `<moq-publish-ui>` element wraps a nested `<moq-publish>`:

```html
<script type="module">
    import "@moq/publish/element";
    import "@moq/publish/ui";
</script>

<moq-publish-ui>
    <moq-publish url="https://relay.example.com/anon" name="room/alice" audio video>
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

## Authentication

Include JWT token in the URL:

```html
<moq-publish
    url="https://relay.example.com/room/123?jwt=eyJhbGciOiJIUzI1NiIs..."
    name="alice"
    audio video>
</moq-publish>
```

See [Authentication](/app/relay/auth) for token generation.

## Next Steps

- Learn about [watching streams](/js/@moq/hang/watch)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Learn about [Web Components](/js/env/web)

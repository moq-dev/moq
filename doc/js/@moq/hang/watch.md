---
title: Watching Streams
description: Subscribe to and render MoQ broadcasts
---

# Watching Streams

This guide covers how to subscribe to and render MoQ broadcasts using `@moq/watch`.

## Web Component

The simplest way to watch a stream:

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

### Attributes

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| `url` | string | required | Relay server URL |
| `name` | string | required | Broadcast name |
| `controls` | boolean | false | Show playback controls |
| `paused` | boolean | false | Pause playback |
| `muted` | boolean | false | Mute audio |
| `volume` | number | 1 | Audio volume (0-1) |

## JavaScript API

For more control, use `@moq/watch` directly:

```typescript
import * as Moq from "@moq/lite";
import * as Watch from "@moq/watch";

const connection = await Moq.Connection.connect(
    new URL("https://relay.example.com/anon")
);

const watch = new Watch.Broadcast({
    connection,
    enabled: true,
    name: "alice",
    reload: true,
});
```

## Playback Controls

### Pause/Resume

```typescript
// Using attribute
watch.setAttribute("paused", "");
watch.removeAttribute("paused");
```

### Volume Control (Web Component)

```typescript
const el = document.querySelector("moq-watch");

// Set volume (0-1)
el.setAttribute("volume", "0.5");

// Mute/unmute
el.setAttribute("muted", "");
el.removeAttribute("muted");
```

## SolidJS Integration

Use `@moq/watch/ui` for the SolidJS UI overlay. The `<moq-watch-ui>` element wraps a nested `<moq-watch>`:

```html
<script type="module">
    import "@moq/watch/element";
    import "@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch url="https://relay.example.com/anon" name="room/alice">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

Or use Web Components directly:

```tsx
import "@moq/watch/element";

function VideoPlayer(props) {
    return (
        <moq-watch
            url={props.url}
            name={props.name}
            controls>
            <canvas />
        </moq-watch>
    );
}
```

## Next Steps

- Learn about [publishing streams](/js/@moq/hang/publish)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Learn about [Web Components](/js/env/web)

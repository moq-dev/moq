---
name: moq
description: Build live video, audio, and real-time data apps with Media over QUIC (MoQ). Use when integrating the @moq/* npm packages or moq-* Rust crates, embedding the <moq-watch> or <moq-publish> web components, publishing media via moq-cli/ffmpeg/OBS/GStreamer, or running a moq-relay server.
---

# Media over QUIC (MoQ)

MoQ is a live media protocol that delivers real-time (sub-second) latency at CDN scale.
It is generic pub/sub over QUIC: video and audio are the flagship use case, but any live data works (chat, game state, JSON snapshots, AI/TTS output).

This file is a map, not the manual. Full documentation lives at https://doc.moq.dev; fetch the linked pages when you need details.

## Architecture

The stack is layered. Pick the highest layer that fits and only drop down when you need control:

1. **Web components** (`<moq-watch>`, `<moq-publish>`): drop-in playback and capture for the browser.
2. **hang** (`@moq/hang`, Rust `hang`): media catalog + container. Codecs, capture, encode, decode, render.
3. **moq-lite** (`@moq/net`, Rust `moq-net`): generic pub/sub transport. Broadcasts contain tracks, tracks contain groups, groups contain frames.
4. **WebTransport / QUIC**: provided by the browser or the `web-transport` crates.

Key rule: the relay (CDN) knows nothing about media or codecs. All business logic lives in the client; content can be end-to-end encrypted.

## Relays

Every client connects to a relay URL. The URL path is an auth scope, and broadcast names are appended to it.

- **Public dev relay**: `https://cdn.moq.dev/anon`. No auth, anyone can publish or subscribe under it. Testing only; use a unique broadcast name to avoid collisions.
- **Local relay**: `cargo install moq-relay` (or brew/apt/dnf/nix/docker), then run with a `relay.toml`. See https://doc.moq.dev/bin/relay/
- **Auth**: JWT tokens signed with `moq-token`, passed as a `?jwt=` query parameter on the connection URL. See https://doc.moq.dev/bin/relay/auth

## Browser: watch a broadcast

Requires WebTransport (Chrome/Edge 97+; Firefox and Safari support is experimental). Works with any bundler, or without one via esm.sh:

```html
<script type="module">
    import "https://esm.sh/@moq/watch/element";
    import "https://esm.sh/@moq/watch/ui";
</script>

<moq-watch-ui>
    <moq-watch url="https://cdn.moq.dev/anon" name="room/alice.hang">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

For a bundler, `npm add @moq/watch` and `import "@moq/watch/element"` instead. Useful attributes: `latency` (default `"real-time"`, or ms), `latency-max` (set above `latency-min` for buffered playback, e.g. TTS streamed faster than real-time), `muted`, `paused`, `controls`. Details: https://doc.moq.dev/lib/js/@moq/watch

## Browser: publish camera or screen

```html
<script type="module">
    import "https://esm.sh/@moq/publish/element";
    import "https://esm.sh/@moq/publish/ui";
</script>

<moq-publish-ui>
    <moq-publish url="https://cdn.moq.dev/anon" name="room/alice.hang" source="camera">
        <video muted autoplay></video>
    </moq-publish>
</moq-publish-ui>
```

`source` is `"camera"`, `"screen"`, or `"file"`. Add `simulcast` for an extra low-res rendition. Details: https://doc.moq.dev/lib/js/@moq/publish

Both elements also expose a full JavaScript API (`Watch.Broadcast`, `Publish.Broadcast`) plus `publishTrack`/`subscribeTrack` for custom data tracks alongside the media.

## Browser/server: raw data with @moq/net

For non-media live data, use `@moq/net` directly (server-side works via a WebTransport polyfill):

```typescript
import * as Moq from "@moq/net";

const connection = await Moq.Connection.connect(new URL("https://cdn.moq.dev/anon"));

// Publish: a broadcast contains named tracks; append groups and frames.
const broadcast = new Moq.Broadcast.Producer();
const track = broadcast.createTrack("chat");
connection.publish(Moq.Path.from("my-broadcast"), broadcast);
const group = track.appendGroup();
group.writeString("Hello MoQ!");
group.close();
```

Runnable examples: https://github.com/moq-dev/moq/tree/main/js/net/examples

## Rust

`moq-native` configures QUIC/TLS, `moq-net` speaks the protocol, `hang` handles media:

```rust
let client = moq_native::ClientConfig::default().init()?;
let url = url::Url::parse("https://cdn.moq.dev/anon")?;

// Subscribe: wire an Origin in before connecting, then await announcements.
let origin = moq_net::Origin::new().produce();
let mut consumer = origin.consume();
let session = client.with_subscriber(origin).connect(url).await?;
while let Some((path, broadcast)) = consumer.announced().await {
    // subscribe to tracks on each broadcast
}
```

Guide: https://doc.moq.dev/lib/rs/env/native. Runnable media examples: https://github.com/moq-dev/moq/tree/main/rs/hang/examples

## Publish media from the command line

The `moq-cli` crate installs a `moq` binary that bridges FFmpeg, HLS, RTMP, SRT, and WebRTC into MoQ:

```bash
ffmpeg -i input.mp4 -c copy -f mpegts - | \
    moq --client-connect https://cdn.moq.dev/anon --broadcast my-stream.hang import ts
```

There are also OBS and GStreamer plugins. See https://doc.moq.dev/bin/

## Gotchas

- **Broadcast naming**: the `.hang` suffix selects the hang catalog format (`.msf` selects MSF). Media broadcasts should end in `.hang`.
- **TLS**: WebTransport requires a valid certificate. Local development against `localhost` uses certificate-hash fetching; production needs a real domain and cert.
- **Latency is a range**: the default minimizes latency and skips ahead. For content written faster than real-time (TTS, server-rendered), raise `latency-max` so playback buffers instead of skipping.
- **Other languages**: Python, Kotlin, Swift, Go, and C bindings wrap the same Rust core. See https://doc.moq.dev/lib/

## Reference

- Concepts and protocol layering: https://doc.moq.dev/concept/
- Relay configuration, auth, clustering: https://doc.moq.dev/bin/relay/
- Rust API docs: https://docs.rs/moq-net and https://docs.rs/hang
- Protocol drafts: https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html

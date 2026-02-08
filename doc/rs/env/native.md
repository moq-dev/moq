---
title: Native
description: Building native MoQ clients in Rust for desktop, mobile, and embedded.
---

# Native

Build native MoQ clients in Rust for desktop, mobile, and embedded platforms.
This guide covers connecting to a relay, discovering broadcasts, subscribing to media tracks, and decoding frames.

## Dependencies

The key crates:

- [moq-native](https://crates.io/crates/moq-native) — Configures QUIC (via [quinn](https://crates.io/crates/quinn)) and TLS (via [rustls](https://crates.io/crates/rustls)) for you.
- [moq-lite](https://crates.io/crates/moq-lite) — The core pub/sub protocol. Can be used directly with any `web_transport_trait::Session` implementation if you need full control over the QUIC endpoint.
- [hang](https://crates.io/crates/hang) — Media-specific catalog and container format on top of `moq-lite`.

## Connecting

Create a [`ClientConfig`](https://docs.rs/moq-native/latest/moq_native/struct.ClientConfig.html) and connect to a relay:

```rust
let client = moq_native::ClientConfig::default().init()?;
let url = url::Url::parse("https://cdn.moq.dev/anon/my-broadcast")?;
let session = client.connect(url).await?;
```

The default configuration uses system TLS roots, enables WebSocket fallback, and gives QUIC a 200ms head-start.

### URL Schemes

The client supports several URL schemes:

- `https://` — WebTransport over HTTP/3 (recommended for browsers and native)
- `http://` — Local development with self-signed certs (fetches the certificate fingerprint automatically)
- `moqt://` — Raw QUIC with the MoQ IETF ALPN (no WebTransport overhead)
- `moql://` — Raw QUIC with the moq-lite ALPN

### Transport Racing

`client.connect()` automatically races QUIC and WebSocket connections.
QUIC gets a configurable head-start (default 200ms); if it fails, WebSocket takes over.
Once WebSocket wins for a given server, future connections skip the delay.
This is transparent to your application.

### Authentication

Pass JWT tokens via URL query parameters:

```rust
let url = Url::parse(&format!(
    "https://relay.example.com/room/123?jwt={}", token
))?;
let session = client.connect(url).await?;
```

See the [Authentication guide](/app/relay/auth) for how to generate tokens.

## Publishing

The [video example](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) demonstrates publishing end-to-end.

The key pattern is: create an [`Origin`](https://docs.rs/moq-lite/latest/moq_lite/struct.Origin.html), connect a session to it, then publish broadcasts:

```rust
let origin = moq_lite::Origin::produce();
let session = client
    .with_publish(origin.consume())
    .connect(url).await?;

let mut broadcast = moq_lite::Broadcast::produce();
// ... add catalog and tracks to the broadcast ...
origin.publish_broadcast("", broadcast.consume());
```

See the full [video.rs](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) example for catalog setup, track creation, and frame encoding.

## Subscribing

The [subscribe example](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs) demonstrates subscribing end-to-end.

To consume a broadcast, use `with_consume()` and listen for announcements:

```rust
let origin = moq_lite::Origin::produce();
let mut consumer = origin.consume();
let session = client
    .with_consume(origin)
    .connect(url).await?;

// Wait for broadcasts to be announced.
while let Some((path, broadcast)) = consumer.announced().await {
    let Some(broadcast) = broadcast else {
        tracing::info!(%path, "broadcast ended");
        continue;
    };
    // Subscribe to tracks on this broadcast...
}
```

If you already know the broadcast path, you can subscribe directly:

```rust
let broadcast = consumer.consume_broadcast("my-stream")
    .expect("broadcast not found");
```

## Reading the Catalog

The [hang](/concept/layer/hang) catalog describes available media tracks.
Subscribe to it using [`CatalogConsumer`](https://docs.rs/hang/latest/hang/struct.CatalogConsumer.html):

```rust
let catalog_track = broadcast.subscribe_track(&hang::Catalog::default_track());
let mut catalog = hang::CatalogConsumer::new(catalog_track);
let info = catalog.next().await?.expect("no catalog");
```

The catalog is live-updated — call `catalog.next().await` again to receive updates when tracks change.

See the full [subscribe.rs](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs) example for iterating renditions and selecting a track.

## Reading Frames

Subscribe to a media track and read frames using [`OrderedConsumer`](https://docs.rs/hang/latest/hang/container/struct.OrderedConsumer.html):

```rust
let track_consumer = broadcast.subscribe_track(&track);
let mut ordered = hang::container::OrderedConsumer::new(
    track_consumer,
    Duration::from_millis(500), // max latency before skipping groups
);

while let Some(frame) = ordered.read().await? {
    // frame.timestamp, frame.keyframe, frame.payload
}
```

`OrderedConsumer` handles group ordering and latency management automatically.
Groups that fall too far behind are skipped to maintain real-time playback.

## Platform Decoders

The frame payload contains the raw codec bitstream.
You need a platform decoder to turn it into pixels or audio samples.

### Video

- **macOS/iOS** — VideoToolbox (`VTDecompressionSession`). Feed H.264 NALs wrapped in `CMSampleBuffer`.
- **Android** — `MediaCodec` via NDK. Feed NAL units directly.
- **Linux** — VA-API via `libva`, or GStreamer for a higher-level API.
- **Cross-platform** — FFmpeg via the `ffmpeg-next` crate works everywhere.

### Audio

For AAC-LC audio, [symphonia](https://crates.io/crates/symphonia) decodes to PCM samples and [cpal](https://crates.io/crates/cpal) handles platform audio output.
For Opus, symphonia also supports decoding, or use the `opus` crate directly.

Use a ring buffer between the decoder and audio output to absorb network jitter.

## Common Pitfalls

### `description` Field in the Catalog

Both `VideoConfig` and `AudioConfig` have a `description` field that provides out-of-band codec initialization data. If present, it contains codec-specific configuration as a hex-encoded byte string.

**Video examples:**
- **H.264** — SPS/PPS in AVCC format. NAL units in the payload are length-prefixed.
- **H.265** — VPS/SPS/PPS in HVCC format.

**Audio examples:**
- **AAC** — `AudioSpecificConfig` bytes.
- **Opus** — Typically `None`; configuration is in-band.

When `description` is `None`, codec parameters are delivered in-band (e.g. Annex B start codes `00 00 00 01` or `00 00 01` for H.264/H.265).
Your decoder must handle whichever format the publisher uses.
See the [hang format docs](/concept/layer/hang) for details.

### Container Format

Check the `container` field for each rendition:

- **`legacy`** — Each frame is a varint timestamp (microseconds) followed by the codec payload. This is the common case.
- **`cmaf`** — Each frame is a `moof` + `mdat` pair (fragmented MP4). Used for HLS compatibility.

`OrderedConsumer` decodes legacy timestamps for you automatically.

## Next Steps

- [hang format](/concept/layer/hang) — Catalog schema and container details
- [moq-lite docs](https://docs.rs/moq-lite) — Core protocol API reference
- [moq-native docs](https://docs.rs/moq-native) — Client configuration options
- [Relay HTTP endpoints](/app/relay/http) — HTTP fetch for debugging and late-join
- [video.rs](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) — Complete publishing example
- [subscribe.rs](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs) — Complete subscribing example

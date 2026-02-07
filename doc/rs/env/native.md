---
title: Native
description: Building native MoQ clients in Rust for desktop, mobile, and embedded.
---

# Native

Build native MoQ clients in Rust for desktop, mobile, and embedded platforms.
This guide covers connecting to a relay, discovering broadcasts, subscribing to media tracks, and decoding frames.

## Dependencies

Add these to your `Cargo.toml`:

```toml
[dependencies]
moq-lite = "0.2"
moq-native = "0.2"
hang = "0.2"
tokio = { version = "1", features = ["full"] }
url = "2"
anyhow = "1"
tracing = "0.1"
```

[moq-native](/rs/crate/moq-native) configures QUIC (via [quinn](https://crates.io/crates/quinn)) and TLS (via [rustls](https://crates.io/crates/rustls)) for you.
If you need full control over the QUIC endpoint, you can use `moq-lite` directly with any `web_transport_trait::Session` implementation.

## Connecting

Create a client and connect to a relay:

```rust
use moq_native::ClientConfig;
use url::Url;

let client = ClientConfig::default().init()?;
let url = Url::parse("https://cdn.moq.dev/anon/my-broadcast")?;
let session = client.connect(url).await?;
```

`ClientConfig` provides sensible defaults: system TLS roots, WebSocket fallback enabled, and a 200ms head-start for QUIC.

### URL Schemes

The client supports several URL schemes:

- `https://` — WebTransport over HTTP/3 (recommended for browsers and native)
- `http://` — Local development with self-signed certs (fetches fingerprint automatically)
- `moql://` — Raw QUIC with the moq-lite ALPN (no WebTransport overhead)

### Transport Racing

`client.connect()` automatically races QUIC and WebSocket connections.
QUIC gets a 200ms head-start; if it fails, WebSocket takes over.
Once WebSocket wins for a given server, future connections skip the delay.
This is transparent to your application.

### TLS

For local development with self-signed certificates:

```rust
let mut config = ClientConfig::default();
config.tls.disable_verify = Some(true); // Don't do this in production
let client = config.init()?;
```

For custom root certificates:

```rust
let mut config = ClientConfig::default();
config.tls.root = vec!["/path/to/root.pem".into()];
let client = config.init()?;
```

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
The key pattern is: create an `Origin`, connect a session to it, then publish broadcasts:

```rust
// Create a local origin for published broadcasts.
let origin = moq_lite::Origin::produce();

// Connect with publishing enabled.
let session = client
    .with_publish(origin.consume())
    .connect(url).await?;

// Create a broadcast and publish it.
let mut broadcast = moq_lite::Broadcast::produce();

// ... add catalog and tracks to the broadcast ...

origin.publish_broadcast("", broadcast.consume());

// Wait until the session closes.
session.closed().await?;
```

## Subscribing

To consume a broadcast, use `with_consume()` and listen for announcements:

```rust
// Create a local origin for incoming broadcasts.
let origin = moq_lite::Origin::produce();
let mut consumer = origin.consume();

// Connect with consuming enabled.
let session = client
    .with_consume(origin.clone())
    .connect(url).await?;

// Wait for broadcasts to be announced.
while let Some((path, broadcast)) = consumer.announced().await {
    let Some(broadcast) = broadcast else {
        tracing::info!(%path, "broadcast ended");
        continue;
    };

    tracing::info!(%path, "new broadcast");
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
Subscribe to it using `CatalogConsumer`:

```rust
use hang::{Catalog, CatalogConsumer};

// Subscribe to the special "catalog.json" track.
let catalog_track = broadcast.subscribe_track(
    &Catalog::default_track()
);
let mut catalog = CatalogConsumer::new(catalog_track);

// Wait for the first catalog update.
let info = catalog.next().await?
    .expect("no catalog received");

// Iterate video renditions.
for (name, config) in &info.video.renditions {
    tracing::info!(
        %name,
        codec = %config.codec,
        width = ?config.coded_width,
        height = ?config.coded_height,
        "video track"
    );
}

// Iterate audio renditions.
for (name, config) in &info.audio.renditions {
    tracing::info!(
        %name,
        codec = %config.codec,
        sample_rate = config.sample_rate,
        channels = config.channel_count,
        "audio track"
    );
}
```

The catalog is live-updated.
Call `catalog.next().await` again to receive updates when tracks change.

## Reading Frames

Subscribe to a media track and read frames using `OrderedConsumer`:

```rust
use hang::container::{OrderedConsumer, Frame};
use std::time::Duration;

// Pick a video track from the catalog.
let track = moq_lite::Track {
    name: "video0".to_string(),
    priority: 1,
};

// Subscribe with a max latency of 500ms.
let track_consumer = broadcast.subscribe_track(&track);
let mut ordered = OrderedConsumer::new(
    track_consumer,
    Duration::from_millis(500),
);

// Read frames in presentation order.
while let Some(frame) = ordered.read().await? {
    let Frame { timestamp, keyframe, payload } = frame;
    let bytes = payload.num_bytes();

    if keyframe {
        tracing::debug!(%timestamp, %bytes, "keyframe");
    }

    // Feed `payload` to your decoder...
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

### Missing Keyframe on Late Join

When joining a live stream mid-broadcast, you may receive delta frames before a keyframe arrives.
Your decoder will produce corrupted output until the next keyframe.

**Workaround**: Use the relay's [HTTP fetch endpoint](/app/relay/http) to request a previous group containing the keyframe, then switch to the live MoQ subscription.

### `description` Field in the Catalog

The `description` field in `VideoConfig` determines how codec parameters are delivered:

- **`description: Some(hex)`** — AVCC format. The hex value contains SPS/PPS (H.264) or VPS/SPS/PPS (H.265). NAL units in the payload are length-prefixed.
- **`description: None`** — Annex B format. SPS/PPS are inline in the bitstream before each keyframe. NAL units use start codes (`00 00 00 01`).

Both formats are valid.
Your decoder must handle whichever format the publisher uses.
See the [hang format docs](/concept/layer/hang) for details.

### Container Format

Check the `container` field for each rendition:

- **`legacy`** — Each frame is a varint timestamp (microseconds) followed by the codec payload. This is the common case.
- **`cmaf`** — Each frame is a `moof` + `mdat` pair (fragmented MP4). Used for HLS compatibility.

`OrderedConsumer` decodes legacy timestamps for you automatically.

## Next Steps

- [hang format](/concept/layer/hang) — Catalog schema and container details
- [moq-lite](/rs/crate/moq-lite) — Core protocol API reference
- [moq-native](/rs/crate/moq-native) — Client configuration options
- [Relay HTTP endpoints](/app/relay/http) — HTTP fetch for debugging and late-join
- [video example](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs) — Complete publishing example

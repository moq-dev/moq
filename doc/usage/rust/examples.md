---
title: Rust Examples
description: Code examples for Rust libraries
---

# Rust Examples

This page provides code examples for common use cases with the Rust libraries.

## Basic Examples

### Simple Chat Application

A complete example of publishing and subscribing to text messages.

**Source:** [moq-native/examples/chat.rs](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs)

```rust
use moq_lite::*;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to relay
    let connection = Connection::connect("https://relay.example.com/chat").await?;

    // Create broadcast and track
    let mut broadcast = BroadcastProducer::new("room-123");
    let mut track = broadcast.create_track("messages");

    // Publish a message
    let mut group = track.append_group();
    group.write(b"Hello from Rust!")?;
    group.close()?;

    connection.publish(&mut broadcast).await?;

    Ok(())
}
```

### Clock Synchronization

Example of timestamp synchronization between publisher and subscriber.

**Source:** [moq-clock](https://github.com/moq-dev/moq/tree/main/rs/moq-clock)

```rust
use moq_clock::*;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Publisher side
    let clock_pub = ClockPublisher::new();
    connection.publish(clock_pub.broadcast()).await?;

    // Subscriber side
    let broadcast = connection.consume("clock").await?;
    let clock_sub = ClockSubscriber::new(broadcast).await?;

    // Get synchronized timestamp
    let timestamp = clock_sub.now();

    Ok(())
}
```

### Video Publishing

Example of publishing video frames using the hang library.

**Source:** [hang/examples/video.rs](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs)

```rust
use hang::*;
use moq_lite::Connection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::connect("https://relay.example.com/demo").await?;

    // Create hang broadcast
    let mut broadcast = Broadcast::new("my-stream");

    // Configure video track
    let mut video = broadcast.create_video_track(VideoConfig {
        codec: "avc1.64002a".to_string(),
        width: 1920,
        height: 1080,
        framerate: 30.0,
        bitrate: 5_000_000,
    })?;

    // Publish keyframe
    video.append_frame(Frame {
        timestamp: 0,
        data: keyframe_data,
        is_keyframe: true,
    })?;

    // Publish delta frames
    video.append_frame(Frame {
        timestamp: 33_333,  // ~30fps
        data: delta_frame_data,
        is_keyframe: false,
    })?;

    connection.publish_broadcast(broadcast).await?;

    Ok(())
}
```

## Advanced Examples

### Custom Priority

Set custom priorities for groups:

```rust
use moq_lite::*;

let mut track = broadcast.create_track("video");

// High priority group (keyframe)
let mut keyframe_group = track.append_group();
keyframe_group.set_priority(100);
keyframe_group.write(keyframe_data)?;
keyframe_group.close()?;

// Normal priority group
let mut delta_group = track.append_group();
delta_group.set_priority(50);
delta_group.write(delta_frame_data)?;
delta_group.close()?;
```

### Partial Reliability

Drop old groups when subscriber is behind:

```rust
use moq_lite::*;
use std::time::Duration;

let mut group = track.append_group();
group.set_expires(Duration::from_secs(2));
group.write(frame_data)?;
group.close()?;
```

If the group isn't delivered within 2 seconds, it will be dropped to maintain real-time latency.

### Error Handling

Handle different error types:

```rust
use moq_lite::*;

match connection.publish(&mut broadcast).await {
    Ok(()) => println!("Published successfully"),
    Err(Error::ConnectionClosed) => {
        eprintln!("Connection closed, reconnecting...");
        // Implement reconnection logic
    }
    Err(Error::InvalidPath(path)) => {
        eprintln!("Invalid path: {}", path);
        // Fix the path
    }
    Err(Error::PermissionDenied) => {
        eprintln!("Permission denied, check JWT token");
        // Request new token
    }
    Err(e) => {
        eprintln!("Unexpected error: {}", e);
        return Err(e.into());
    }
}
```

### Multi-Track Publishing

Publish multiple tracks in one broadcast:

```rust
use hang::*;

let mut broadcast = Broadcast::new("conference");

// Video track
let mut video = broadcast.create_video_track(VideoConfig {
    codec: "avc1.64002a".to_string(),
    width: 1280,
    height: 720,
    framerate: 30.0,
    bitrate: 2_500_000,
})?;

// Audio track
let mut audio = broadcast.create_audio_track(AudioConfig {
    codec: "opus".to_string(),
    sample_rate: 48000,
    channels: 2,
    bitrate: 128_000,
})?;

// Chat track (text)
let mut chat = broadcast.create_track("chat");

// Publish frames to each track
video.append_frame(video_frame)?;
audio.append_frame(audio_packet)?;
chat.append_group().write(b"Hello!")?;

connection.publish_broadcast(broadcast).await?;
```

### Subscribing to Multiple Tracks

Subscribe to all tracks in a broadcast:

```rust
use hang::*;

let connection = Connection::connect("https://relay.example.com/demo").await?;
let broadcast = connection.consume_broadcast("conference").await?;

// Read catalog to discover tracks
let catalog = broadcast.catalog().await?;

// Spawn tasks for each track
for track_info in catalog.tracks {
    let broadcast_clone = broadcast.clone();

    tokio::spawn(async move {
        let mut track = broadcast_clone.subscribe(&track_info.name).await?;

        while let Some(frame) = track.next_frame().await? {
            match track_info.kind.as_str() {
                "video" => handle_video_frame(frame),
                "audio" => handle_audio_frame(frame),
                _ => handle_other_frame(frame),
            }
        }

        Ok::<(), Error>(())
    });
}
```

### CMAF Import

Import existing fMP4/CMAF files:

```rust
use hang::cmaf::*;
use std::fs;

// Read CMAF file
let data = fs::read("video.mp4")?;

// Convert to hang broadcast
let broadcast = Broadcast::from_cmaf(&data)?;

// Publish to relay
connection.publish_broadcast(broadcast).await?;
```

### Custom QUIC Configuration

Use custom QUIC settings:

```rust
use moq_lite::*;
use quinn::*;

// Create custom QUIC client config
let mut client_config = ClientConfig::new(Arc::new(rustls_config));
client_config.transport_config(Arc::new({
    let mut config = TransportConfig::default();
    config.max_concurrent_uni_streams(1000u32.into());
    config.max_concurrent_bidi_streams(100u32.into());
    config
}));

// Create connection with custom config
let connection = Connection::connect_with_config(
    "https://relay.example.com/demo",
    client_config
).await?;
```

## Testing Examples

### Mock Relay for Testing

Create a mock relay for unit tests:

```rust
use moq_lite::*;

#[tokio::test]
async fn test_publish_subscribe() {
    // Create in-memory relay
    let relay = MockRelay::new();

    // Connect publisher
    let pub_conn = relay.connect("test").await.unwrap();
    let mut broadcast = BroadcastProducer::new("test-broadcast");
    let mut track = broadcast.create_track("test-track");

    // Publish data
    let mut group = track.append_group();
    group.write(b"test data").unwrap();
    group.close().unwrap();
    pub_conn.publish(&mut broadcast).await.unwrap();

    // Connect subscriber
    let sub_conn = relay.connect("test").await.unwrap();
    let broadcast = sub_conn.consume("test-broadcast").await.unwrap();
    let mut track = broadcast.subscribe("test-track").await.unwrap();

    // Read data
    let group = track.next_group().await.unwrap().unwrap();
    let data = group.read().await.unwrap().unwrap();

    assert_eq!(data, b"test data");
}
```

## More Examples

For more examples, see the repository:

- [Rust examples directory](https://github.com/moq-dev/moq/tree/main/rs)
- [moq-lite examples](https://github.com/moq-dev/moq/tree/main/rs/moq-lite/examples)
- [hang examples](https://github.com/moq-dev/moq/tree/main/rs/hang/examples)
- [moq-native examples](https://github.com/moq-dev/moq/tree/main/rs/moq-native/examples)

## Next Steps

- Read the [moq-lite API](/rust/moq-lite)
- Read the [hang API](/rust/hang)
- View [API reference](/api/rust)
- Check out [TypeScript examples](/typescript/examples)

---
title: libmoq
description: C bindings for MoQ
---

# libmoq

[![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)

C bindings for MoQ via FFI, providing media publish/subscribe functionality for C/C++ applications and other languages.

## Overview

`libmoq` provides a C API for real-time media delivery over QUIC. It wraps the Rust [moq-lite](/rs/crate/moq-lite) and [moq-mux](/rs/crate/moq-mux) crates, handling:

- **Sessions** - QUIC/WebTransport connections to MoQ relays
- **Origins** - Containers for broadcast discovery and routing
- **Publishing** - Encoding and sending audio/video tracks
- **Consuming** - Receiving, decoding, and rendering media tracks
- **Catalogs** - Discovering available audio/video renditions

All functions use opaque integer handles to reference resources. Negative return values indicate errors, zero indicates success, and positive values are resource handles.

## Installation

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq/rs/libmoq
cargo build --release
```

The static library will be at `target/release/libmoq.a`.

### Linking

With CMake:

```cmake
find_package(moq REQUIRED)
target_link_libraries(myapp moq)
```

With pkg-config:

```bash
gcc -o myapp myapp.c $(pkg-config --cflags --libs moq)
```

## API

### Initialization

| Function | Description |
|----------|-------------|
| `moq_log_level(level, level_len)` | Set log level: `"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"` |

### Sessions

Connect to a MoQ relay server over QUIC/WebTransport.

| Function | Description |
|----------|-------------|
| `moq_session_connect(url, url_len, origin_publish, origin_consume, on_status, user_data)` | Connect to a relay. Provide origin handles for publish/consume, or `0` to disable. Calls `on_status` on connect (code 0) and close (code non-zero). |
| `moq_session_close(session)` | Close a session and cancel its background task. |

### Origins

Origins group broadcasts by path. They can be shared across sessions for fanout/relaying.

| Function | Description |
|----------|-------------|
| `moq_origin_create()` | Create a new origin. |
| `moq_origin_publish(origin, path, path_len, broadcast)` | Publish a broadcast to an origin at the given path. |
| `moq_origin_consume(origin, path, path_len)` | Consume a broadcast from an origin by path. Returns a broadcast handle. |
| `moq_origin_announced(origin, on_announce, user_data)` | Discover broadcasts published to an origin. Calls `on_announce` with an announced ID for each broadcast. |
| `moq_origin_announced_info(announced, dst)` | Query the path and active status of an announced broadcast. |
| `moq_origin_announced_close(announced)` | Stop listening for announcements. |
| `moq_origin_close(origin)` | Close an origin. |

### Publishing

Create broadcasts and write media frames.

| Function | Description |
|----------|-------------|
| `moq_publish_create()` | Create a new broadcast for publishing. |
| `moq_publish_media_ordered(broadcast, format, format_len, init, init_size)` | Add a media track to a broadcast. `format` specifies the encoding. `init` is codec-specific initialization data. |
| `moq_publish_media_frame(media, payload, payload_size, timestamp_us)` | Write a frame to a media track. Frames must be in decode order. Timestamp is in microseconds. |
| `moq_publish_media_close(media)` | Remove a media track from a broadcast. |
| `moq_publish_close(broadcast)` | Close a broadcast. |

### Consuming

Subscribe to broadcasts and receive decoded media frames.

| Function | Description |
|----------|-------------|
| `moq_consume_catalog_subscribe(broadcast, on_catalog, user_data)` | Subscribe to catalog updates. Calls `on_catalog` with a catalog snapshot ID when the catalog changes. |
| `moq_consume_catalog_unsubscribe(catalog)` | Stop the catalog subscription background task. Previously delivered snapshots remain valid. |
| `moq_consume_catalog_close(catalog)` | Close a catalog snapshot. Invalidates any borrowed pointers from config queries. |
| `moq_consume_video_config(catalog, index, dst)` | Query video rendition info: name, codec, description, dimensions. |
| `moq_consume_audio_config(catalog, index, dst)` | Query audio rendition info: name, codec, description, sample rate, channels. |
| `moq_consume_video_ordered(catalog, index, max_latency_ms, on_frame, user_data)` | Subscribe to a video track. Delivers frames in order, skipping GoPs when latency exceeds `max_latency_ms`. |
| `moq_consume_audio_ordered(catalog, index, max_latency_ms, on_frame, user_data)` | Subscribe to an audio track. Same latency behavior as video. |
| `moq_consume_video_close(track)` | Close a video track subscription. |
| `moq_consume_audio_close(track)` | Close an audio track subscription. |
| `moq_consume_frame_chunk(frame, index, dst)` | Read a chunk of frame payload. Call with increasing `index` to get all chunks. |
| `moq_consume_frame_close(frame)` | Close a frame and release its memory. |
| `moq_consume_close(consume)` | Close a broadcast consumer. |

### Data Structures

```c
// Video rendition configuration
typedef struct {
    const char *name;       // Track name (NOT null-terminated)
    size_t name_len;
    const char *codec;      // Codec string (NOT null-terminated)
    size_t codec_len;
    const uint8_t *description;  // Codec-specific init data, or NULL
    size_t description_len;
    const uint32_t *coded_width;   // Encoded width, or NULL
    const uint32_t *coded_height;  // Encoded height, or NULL
} moq_video_config;

// Audio rendition configuration
typedef struct {
    const char *name;
    size_t name_len;
    const char *codec;
    size_t codec_len;
    const uint8_t *description;  // Codec-specific init data, or NULL
    size_t description_len;
    uint32_t sample_rate;    // Sample rate in Hz
    uint32_t channel_count;  // Number of channels
} moq_audio_config;

// A frame of media data
typedef struct {
    const uint8_t *payload;  // Frame data, or NULL if stream ended
    size_t payload_size;
    uint64_t timestamp_us;   // Presentation timestamp in microseconds
    bool keyframe;           // True if this starts a new GoP
} moq_frame;

// An announced broadcast
typedef struct {
    const char *path;    // Broadcast path (NOT null-terminated)
    size_t path_len;
    bool active;         // Whether the broadcast is currently active
} moq_announced;
```

## Usage Example

### Publishing

```c
#include <moq.h>

// Initialize logging
moq_log_level("info", 4);

// Create origin and session
int origin = moq_origin_create();
int session = moq_session_connect(url, url_len, origin, 0, on_status, NULL);

// Create a broadcast with a video track
int broadcast = moq_publish_create();
int video = moq_publish_media_ordered(broadcast, "h264", 4, init_data, init_size);

// Write frames
moq_publish_media_frame(video, frame_data, frame_size, timestamp_us);

// Publish to the relay
moq_origin_publish(origin, "my-stream", 9, broadcast);
```

### Consuming

```c
// Create origin and connect
int origin = moq_origin_create();
int session = moq_session_connect(url, url_len, 0, origin, on_status, NULL);

// Consume a broadcast
int broadcast = moq_origin_consume(origin, "my-stream", 9);

// Subscribe to the catalog
moq_consume_catalog_subscribe(broadcast, on_catalog, NULL);

// In the catalog callback:
void on_catalog(void *user_data, int catalog) {
    moq_video_config config;
    moq_consume_video_config(catalog, 0, &config);

    // Subscribe to the first video track
    moq_consume_video_ordered(catalog, 0, 500, on_frame, NULL);
}

// In the frame callback:
void on_frame(void *user_data, int frame) {
    moq_frame chunk;
    moq_consume_frame_chunk(frame, 0, &chunk);
    // process chunk.payload, chunk.timestamp_us, chunk.keyframe
    moq_consume_frame_close(frame);
}
```

## Next Steps

- Use [moq-lite](/rs/crate/moq-lite) for Rust applications
- Deploy a [relay server](/app/relay/)
- Read the [Concepts guide](/concept/)

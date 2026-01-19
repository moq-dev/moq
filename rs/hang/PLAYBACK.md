# Native Playback Support

The `hang` crate now includes optional native playback support through the `decode` and `render` modules.

> **⚠️ Integration in Progress**: This module is being enhanced with production-ready encoding/decoding from [moq-media](https://github.com/n0-computer/iroh-live/tree/main/moq-media). The current implementation provides basic functionality. See [MOQ_MEDIA_INTEGRATION.md](MOQ_MEDIA_INTEGRATION.md) for details on upcoming improvements including hardware acceleration, encoding support, and better performance.

## Features

### `decode` - Frame Decoding (FFmpeg)

Decodes compressed media frames to raw audio/video data.

**Supported Codecs:**
- Video: H.264, H.265, VP8, VP9, AV1
- Audio: AAC, Opus

**Requirements:**
- System FFmpeg libraries (`libavcodec`, `libavutil`, `libavformat`)
- On Debian/Ubuntu: `sudo apt install libavcodec-dev libavutil-dev libavformat-dev`
- On macOS: `brew install ffmpeg`

**Usage:**

```rust
use hang::decode::{VideoDecoder, Decoder};
use hang::catalog::video::VideoCodec;

// Create decoder
let mut decoder = VideoDecoder::new(VideoCodec::H264(Default::default()), None)?;

// Decode frame from TrackConsumer
let decoded = decoder.decode(&frame)?;

// Access raw YUV data
if let DecodedFrame::Video(video_frame) = decoded {
    println!("Decoded {}x{} frame", video_frame.width, video_frame.height);
    // video_frame.planes contains Y, U, V planes
}
```

### `render` - GPU Rendering (wgpu)

Renders decoded video frames using GPU acceleration with automatic YUV to RGB conversion.

**Supported Formats:**
- YUV420P (most common)
- YUV422P
- YUV444P

**Requirements:**
- GPU with Vulkan/Metal/DirectX support
- wgpu runtime

**Usage:**

```rust
use hang::render::{VideoRenderer, Renderer};

// Create renderer with a window
let mut renderer = VideoRenderer::new(window, width, height).await?;

// Render decoded frames
renderer.render(&video_frame)?;

// Handle window resize
renderer.resize(new_width, new_height)?;
```

### `playback` - Combined Feature

Convenience feature that enables both `decode` and `render`.

## Enabling Features

In your `Cargo.toml`:

```toml
[dependencies]
hang = { version = "0.10", features = ["playback"] }
```

Or individually:

```toml
hang = { version = "0.10", features = ["decode"] }  # Decoding only
hang = { version = "0.10", features = ["render"] }  # Rendering only
```

## hang-cli Support

The `hang` CLI tool can be built with playback support:

```bash
cd rs/hang-cli
cargo build --features playback
```

**Note:** Requires system FFmpeg installation.

## Architecture & Future Plans

### Current Implementation

```
Encoded Frame (H.264/AAC)
    ↓
decode::VideoDecoder (FFmpeg)
    ↓
VideoFrame (YUV420P)
    ↓
render::VideoRenderer (wgpu)
    ↓
Display (Vulkan/Metal/DX)
```

### Future: WebCodecs Abstraction

The `Decoder` trait is designed to support multiple backends:

```rust
#[cfg(target_family = "wasm")]
use webcodecs::Decoder;  // Browser WebCodecs API

#[cfg(target_os = "ios")]
use videotoolbox::Decoder;  // iOS VideoToolbox

#[cfg(target_os = "android")]
use mediacodec::Decoder;  // Android MediaCodec

#[cfg(not(any(target_family = "wasm", target_os = "ios", target_os = "android")))]
use ffmpeg::Decoder;  // Desktop FFmpeg
```

This allows the same application code to work across:
- **Web**: Using browser's native WebCodecs API
- **iOS**: Using VideoToolbox for hardware acceleration
- **Android**: Using MediaCodec for hardware acceleration
- **Desktop**: Using FFmpeg (current implementation)

### Rendering Plans

- **Web**: Canvas 2D / WebGL / WebGPU rendering
- **iOS**: AVSampleBufferDisplayLayer or Metal
- **Android**: SurfaceView or TextureView
- **Desktop**: wgpu (current implementation)

## Example: Complete Playback Pipeline

```rust
use hang::{BroadcastConsumer, TrackConsumer};
use hang::decode::{VideoDecoder, Decoder, DecodedFrame};
use hang::render::{VideoRenderer, Renderer};
use hang::catalog::video::VideoCodec;

async fn playback_video(
    mut track: TrackConsumer,
    window: impl HasWindowHandle + HasDisplayHandle,
) -> anyhow::Result<()> {
    // Create decoder
    let mut decoder = VideoDecoder::new(
        VideoCodec::H264(Default::default()),
        None
    )?;

    // Create renderer
    let mut renderer = VideoRenderer::new(window, 1920, 1080).await?;

    // Consume and render frames
    while let Some(group) = track.next_group().await? {
        while let Some(frame) = group.next_frame().await? {
            // Decode frame
            let decoded = decoder.decode(&frame)?;

            // Render to screen
            if let DecodedFrame::Video(video_frame) = decoded {
                renderer.render(&video_frame)?;
            }
        }
    }

    Ok(())
}
```

## Performance Considerations

### Decoding
- FFmpeg uses software decoding by default
- Future: Platform-specific hardware decoders (VideoToolbox, MediaCodec)
- Consider frame buffering for smooth playback

### Rendering
- wgpu automatically uses GPU acceleration (Vulkan/Metal/DirectX)
- YUV to RGB conversion happens on GPU via compute shader
- vsync enabled by default (PresentMode::Fifo)

## Limitations

### Current
- No hardware video decode acceleration (FFmpeg software only)
- Video rendering only (audio playback not yet implemented)
- No audio synchronization
- No adaptive quality selection

### Planned
- Hardware decode via platform APIs
- Audio playback
- A/V sync
- Adaptive bitrate support

## Contributing

The playback modules are designed to be extensible. Key areas for contribution:

1. **Platform-specific decoders**: VideoToolbox (iOS), MediaCodec (Android), VA-API (Linux)
2. **Audio playback**: Cross-platform audio output
3. **A/V synchronization**: Timestamp-based sync
4. **WebCodecs backend**: Browser support

See the trait definitions in `decode/mod.rs` and `render/mod.rs` for the abstraction interfaces.

# moq-media Integration Plan

This document outlines the integration of [moq-media](https://github.com/n0-computer/iroh-live/tree/main/moq-media) into the hang crate.

## Current State

The hang crate currently has:

1. **av.rs** - Trait abstractions adapted from moq-media for codec operations
2. **decode/** - Simple FFmpeg 7-based decoders (YUV/PCM output)
3. **render/** - wgpu-based video rendering with YUV→RGB conversion
4. Feature flags: `decode`, `encode`, `render`, `playback`, `full`

## moq-media Capabilities

moq-media provides production-ready encoding/decoding with:

### Encoding
- **H.264** with hardware acceleration:
  - Linux: VAAPI (primary), NVENC, QSV
  - macOS: VideoToolbox
  - Windows: NVENC, QSV, AMF
  - Fallback: libx264 (software)
- **Opus** audio encoding (HQ: 128kbps, LQ: 32kbps)
- Quality presets (180p, 360p, 720p, 1080p @ 30fps)
- Automatic bitrate calculation
- MP4-compatible output (avcC extradata)

### Decoding
- **H.264** and **AV1** video decoding
- **Opus** audio decoding
- Viewport-aware rescaling (maintains aspect ratio)
- Frame timestamp management
- RGBA output for easy rendering

### Additional Features
- Thread-per-track encoding/decoding
- Capture support (camera via nokhwa, screen via xcap)
- Audio playback engine (Firewheel with AEC)
- Rendition management for adaptive bitrate

## Integration Path

### Phase 1: Trait Alignment (Current)

✅ Created `av.rs` with moq-media's trait abstractions:
- `AudioEncoder` / `AudioDecoder`
- `VideoEncoder` / `VideoDecoder`
- `AudioFormat`, `VideoFormat`, `VideoFrame`
- `VideoPreset`, `AudioPreset`
- `DecodeConfig`

✅ Updated Cargo.toml:
- FFmpeg 8.0 (from 7.0)
- Added `image` crate for RgbaImage
- Added `encode` feature flag

### Phase 2: moq-media Integration (Pending Contribution)

When moq-media is contributed, integrate:

1. **Copy FFmpeg implementations**:
   ```
   moq-media/src/ffmpeg/
   ├── mod.rs           → hang/src/ffmpeg/mod.rs
   ├── ext.rs           → hang/src/ffmpeg/ext.rs
   ├── video/
   │   ├── encoder.rs   → hang/src/encode/video.rs (impl av::VideoEncoder)
   │   ├── decoder.rs   → hang/src/decode/video.rs (impl av::VideoDecoder)
   │   └── util.rs      → hang/src/ffmpeg/video_util.rs
   └── audio/
       ├── encoder.rs   → hang/src/encode/audio.rs (impl av::AudioEncoder)
       └── decoder.rs   → hang/src/decode/audio.rs (impl av::AudioDecoder)
   ```

2. **Update module structure**:
   ```
   rs/hang/src/
   ├── av.rs            # Trait abstractions (done)
   ├── ffmpeg/          # FFmpeg utilities (new)
   │   ├── mod.rs
   │   ├── ext.rs
   │   └── video_util.rs
   ├── encode/          # Encoding implementations (new)
   │   ├── mod.rs
   │   ├── video.rs     # H264Encoder (from moq-media)
   │   └── audio.rs     # OpusEncoder (from moq-media)
   ├── decode/          # Decoding implementations (replace simple ones)
   │   ├── mod.rs
   │   ├── video.rs     # FfmpegVideoDecoder (from moq-media)
   │   └── audio.rs     # FfmpegAudioDecoder (from moq-media)
   └── render/          # Video rendering (keep current)
       ├── mod.rs
       ├── video.rs
       └── shaders/video.wgsl
   ```

3. **Update Cargo.toml dependencies**:
   ```toml
   # Hardware acceleration per platform
   [target.'cfg(target_os = "linux")'.dependencies.ffmpeg-sys-next]
   version = "8"
   optional = true
   features = ["vaapi"]

   [target.'cfg(target_os = "macos")'.dependencies.ffmpeg-sys-next]
   version = "8"
   optional = true
   features = ["videotoolbox"]
   ```

### Phase 3: Capture Integration (Optional)

If desired, integrate moq-media's capture capabilities:

```
moq-media/src/
├── capture.rs       → hang/src/capture/mod.rs
├── publish.rs       → hang/src/publish/mod.rs (or keep in moq-media)
└── subscribe.rs     → hang/src/subscribe/mod.rs (or keep in moq-media)
```

**Note**: Capture/publish/subscribe may be better suited for a separate `moq-media` crate that depends on `hang`, rather than bundling everything into hang.

### Phase 4: Unification

Decide on final crate structure:

**Option A: Monorepo with separate crates**
```
rs/
├── hang/          # Protocol + catalog + import (current)
├── moq-media/     # Capture + encode + decode + playback
└── hang-cli/      # CLI tool using both
```

**Option B: Integrated crate**
```
rs/hang/           # Everything in one crate with feature flags
├── features: core, encode, decode, render, capture, playback, full
└── Optional deps gated per feature
```

## Architecture Comparison

### Current hang decode module
- Simple push/pop API: `decode(&Frame) -> Result<DecodedFrame>`
- Direct YUV plane access
- Software decoding only
- 140 lines of code

### moq-media FFmpeg decoder
- Push/pop separation: `push_packet()` then `pop_frame()`
- RGBA output via `image::Frame`
- Hardware acceleration support (VAAPI, VideoToolbox, etc.)
- Viewport-aware rescaling
- 140 lines of code (similar complexity)

**Recommendation**: Replace current decode module with moq-media's implementation.

### Current hang render module
- wgpu-based GPU rendering
- YUV→RGB shader conversion (BT.709)
- Cross-platform (Vulkan/Metal/DirectX)
- ~550 lines of code

### moq-media (no render module)
- Audio playback only (Firewheel engine)
- No video rendering

**Recommendation**: Keep current render module, it fills a gap moq-media doesn't address.

## Migration Path for Users

If users are currently using the simple decode module, provide migration guide:

```rust
// Old API (current)
use hang::decode::{VideoDecoder, Decoder};
let mut decoder = VideoDecoder::new(codec, None)?;
let decoded = decoder.decode(&frame)?;

// New API (moq-media)
use hang::av::{VideoDecoder, DecodeConfig};
use hang::decode::video::FfmpegVideoDecoder;

let config = DecodeConfig::default();
let mut decoder = FfmpegVideoDecoder::new(&video_config, &config)?;
decoder.push_packet(frame)?;
if let Some(decoded) = decoder.pop_frame()? {
    // Use decoded.img() for RGBA data
}
```

## Testing Strategy

1. **Unit tests**: Test encoders/decoders independently
2. **Integration tests**: Encode→Decode round-trip
3. **Benchmark**: Compare software vs hardware encode/decode performance
4. **Platform tests**: Verify hardware acceleration on each platform

## Documentation Updates

1. Update PLAYBACK.md with new architecture
2. Add hardware acceleration docs
3. Add encoding examples
4. Update README.md feature matrix
5. Add moq-media attribution and license info

## Open Questions

1. **Crate structure**: One crate or split moq-media into separate crate?
2. **Capture/publish**: Include in hang or keep separate?
3. **Audio playback**: Integrate Firewheel or leave as separate concern?
4. **Threading**: Adopt moq-media's thread-per-track model or leave to users?
5. **Licensing**: moq-media may have different license requirements for FFmpeg features

## Timeline

1. ✅ **Phase 1**: Trait abstractions (current commit)
2. **Phase 2**: Await moq-media contribution
3. **Phase 3**: Integration and testing (1-2 weeks)
4. **Phase 4**: Documentation and examples (1 week)
5. **Phase 5**: Release with breaking changes (0.11.0)

## Attribution

moq-media is developed by the [Number Zero](https://github.com/n0-computer) team as part of the [iroh-live](https://github.com/n0-computer/iroh-live) project. This integration plan respects their work and aims for a clean, well-documented merge.

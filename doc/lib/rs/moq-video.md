---
title: moq-video
description: Native capture, hardware codecs, and GPU rendering
---

# moq-video

[![crates.io](https://img.shields.io/crates/v/moq-video)](https://crates.io/crates/moq-video)
[![docs.rs](https://docs.rs/moq-video/badge.svg)](https://docs.rs/moq-video)

What a browser gets from `getUserMedia` and WebCodecs, for native Rust: no
ffmpeg, no GStreamer, no system codec to install.

| Module | Does | Backends |
| --- | --- | --- |
| `capture` | Camera, display, window, or application frames | AVFoundation + ScreenCaptureKit (macOS), V4L2 + X11/portal + PipeWire (Linux), Media Foundation + DXGI (Windows) |
| `encode` | Frames to H.264/H.265, published as a hang track | VideoToolbox, Media Foundation, NVENC, VAAPI, V4L2 M2M, MediaCodec (Android), openh264 |
| `decode` | A subscribed track back to frames | VideoToolbox, Media Foundation/DXVA, NVDEC, V4L2 M2M, MediaCodec (Android), openh264 |
| `render` | A frame as a `wgpu` texture | wgpu, with zero-copy Metal and Vulkan imports |

Highlights:

- **Automatic backend selection**, hardware first. Linux GPU libraries are `dlopen`ed at runtime, so one binary starts anywhere and warns when it falls back to software. openh264 is statically linked as the H.264 fallback; H.265 is hardware-only; AV1 decodes via NVDEC. The VAAPI encoder is compile-verified but not yet validated on hardware.
- **Publish on demand.** `encode::publish_capture` advertises the track up front and opens the camera only while someone subscribes.
- **Zero-copy where the platform allows.** Matching codec backends consume their native GPU surfaces directly. The renderer imports `CVPixelBuffer` and supported DMA-BUF formats; other combinations use the universal `Surface::into_i420()` and `into_rgba()` CPU exits.
- **Live bitrate control** where the selected backend supports it, without forcing a keyframe. An unsupported backend keeps its opening rate.
- **Device enumeration** for cameras, displays, windows, and apps, matching `moq devices`.

```rust
let mut video = moq_video::decode::Consumer::new(&broadcast, &rendition, "video", Default::default()).await?;
let mut renderer = moq_video::render::Renderer::new(&device, &queue, Default::default())?;
while let Some(frame) = video.read().await? {
    let texture = renderer.render(&frame)?;
}
```

```bash
cargo add moq-video                      # capture, nvidia, vaapi, v4l2, mediacodec, render on by default
cargo add moq-video --features pipewire  # Wayland screen capture (links libpipewire)
cargo add moq-video --no-default-features  # codec-only: still encodes/decodes H.264
```

The language bindings use the codec-only shape, which is what keeps their
Android floor at API 24 instead of MediaCodec's API 26 entry points.

API: [docs.rs/moq-video](https://docs.rs/moq-video). Pair with
[`moq-audio`](/lib/rs/moq-audio).

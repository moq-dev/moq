---
title: moq-video
description: Native capture, hardware codecs, and GPU rendering
---

# moq-video

[![crates.io](https://img.shields.io/crates/v/moq-video)](https://crates.io/crates/moq-video)
[![docs.rs](https://docs.rs/moq-video/badge.svg)](https://docs.rs/moq-video)

What a browser gets from `getUserMedia` and WebCodecs, for native Rust: no
ffmpeg, no GStreamer, no system codec to install. `moq import capture` and
`moq play` are built on it.

| Module | Does | Backends |
| --- | --- | --- |
| `capture` | Camera, display, window, or application frames | AVFoundation + ScreenCaptureKit (macOS), V4L2 + X11/portal + PipeWire (Linux), Media Foundation + Windows.Graphics.Capture (Windows) |
| `encode` | Frames to H.264/H.265, published as a hang track | VideoToolbox, Media Foundation, NVENC, VAAPI, V4L2 M2M, MediaCodec (Android), openh264 |
| `decode` | A subscribed track back to frames | VideoToolbox, Media Foundation/DXVA, NVDEC, VAAPI, V4L2 M2M, MediaCodec (Android), openh264, libvpx |
| `render` | A frame as a `wgpu` texture | wgpu, with zero-copy Metal and Vulkan imports |

Selection is automatic and hardware first. GPU libraries are loaded at
runtime, so one binary starts anywhere and warns when it falls back to
software. openh264 is the H.264 fallback and is on by default. H.265 is
hardware-only, AV1 decodes through NVDEC and MediaCodec, and VP8 and VP9
decode in software through the opt-in `vpx` feature.

Capture is pull-driven: `encode::publish_capture` advertises the track up
front and opens the camera only while someone subscribes. Backends that can
change bitrate live follow the connection's send estimate. Where the platform
allows it, frames stay on the GPU between capture, decode, encode, and render.

Opt-in features: `capture`, `vaapi` (Intel and AMD on Linux; the build needs
libclang), `v4l2` (ARM SoC codecs such as a Raspberry Pi's), `pipewire` (Linux
portal capture and PipeWire cameras; links libpipewire), and `vpx` (links
libvpx).

Windows display and window capture use Windows.Graphics.Capture on Windows 10
2004 or newer; application capture and system audio are not provided there.

API: [docs.rs/moq-video](https://docs.rs/moq-video). Pair with
[`moq-audio`](/lib/rs/moq-audio).

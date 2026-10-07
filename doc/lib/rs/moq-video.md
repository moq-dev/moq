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

Selection is automatic and hardware first. Linux GPU libraries are `dlopen`ed,
so one binary starts anywhere and warns when it falls back to software.
openh264 is the H.264 fallback and is on by default. H.265 is hardware-only.
AV1 decodes through NVDEC. VP8 and VP9 decode in software through the opt-in
`vpx` feature, 8-bit 4:2:0 only.

`encode::publish_capture` advertises the track up front and opens the camera
only while someone subscribes. Backends that can change bitrate live follow
the connection's send estimate without forcing a keyframe. `cut()` asks for a
keyframe and returns an error on a backend that cannot force one, rather than
quietly waiting for the GOP.

Where the platform allows it, frames stay on the GPU: the renderer imports
`CVPixelBuffer` and supported DMA-BUF formats, and an NVIDIA path can pass
Vulkan frames to NVENC without a CPU copy. Vulkan and CUDA surfaces have no
CPU fallback; other surfaces can be read back.

`encode::Config::preset` is `LowLatency` (the default), `Balanced`, or
`Quality`. The encoder reports the preset whose controls actually took effect.
Only NVENC and openh264 change compression with the preset. VideoToolbox and
VAAPI have one low-latency mode and report that. Media Foundation, MediaCodec,
and V4L2 do not report one. A preset does not change keyframe interval or
viewer buffering.

## Windows

Display and window capture need Windows 10 2004 (build 19041) or newer and use
Windows.Graphics.Capture. There is no Desktop Duplication fallback. The system
capture border stays unless the OS grants borderless capture (build 20348 or
newer). Application capture and system audio are not provided.
`display:N` is an enumeration index, not a stable monitor id, so a saved
selector can name a different screen after an upgrade. Enumerate again with
`moq devices`. A resize or a closed window ends the stream so the caller can
reopen. If no frame arrives within five seconds, opening fails.

## Linux

Local X11 display and window capture uses shared memory. Remote displays and
servers without it use `GetImage`, and a failure is reported rather than
switched over silently. A settled resize ends the stream. Unmapped windows
hold capture until they are viewable again.

Intel QuickSync is the opt-in `vaapi` feature, off by default because the
build needs libclang. It encodes and decodes 8-bit H.264. At runtime, libva
and the Intel `iHD` driver must be installed and the user must be able to open
`/dev/dri/renderD*`. `MOQ_VAAPI_DEVICE` picks a node when several GPUs are
present. Automatic selection uses VAAPI when it opens, and H.264 can still
fall back to openh264. Naming the backend requires it.

PipeWire cameras and `--display` on the CLI need the `pipewire` feature.

API: [docs.rs/moq-video](https://docs.rs/moq-video). Pair with
[`moq-audio`](/lib/rs/moq-audio).

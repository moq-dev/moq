---
title: moq-video
description: Native video capture, encoding, decoding, and GPU rendering for MoQ
---

# moq-video

[![crates.io](https://img.shields.io/crates/v/moq-video)](https://crates.io/crates/moq-video)
[![docs.rs](https://docs.rs/moq-video/badge.svg)](https://docs.rs/moq-video)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

The native video stack: grab pictures from a camera or a screen, encode them with
a hardware codec, publish them as a [hang](/lib/rs/crate/hang) track, and do the
same in reverse down to a texture on screen.

This is what the browser gets from WebCodecs and `getUserMedia`. A native
application has no such thing, so `moq-video` provides it: no ffmpeg, no GStreamer,
just the platform APIs and a couple of pure-Rust fallbacks.

## Overview

Four role modules, symmetric on both ends of the wire:

| Module | Does | Platform |
| --- | --- | --- |
| `capture` | Camera, display, window, or application frames | AVFoundation + ScreenCaptureKit (macOS), V4L2 + PipeWire (Linux), Media Foundation + DXGI (Windows) |
| `encode` | Raw frames to H.264/H.265, published through `moq-mux` | VideoToolbox, Media Foundation, NVENC, VAAPI, openh264 |
| `decode` | A subscribed track back to raw frames | VideoToolbox, Media Foundation/DXVA, NVDEC, openh264 |
| `render` | A frame drawn on the GPU, handed back as a `wgpu` texture | wgpu, with a zero-copy Metal import on macOS |

A picture is a `Frame` wherever it crosses the API: a `moq_net::Timestamp` and a
`Surface` holding the pixels. Capture and decode produce them, encode and render
consume them.

Backend selection is automatic and degrading. Every hardware library is loaded at
runtime (`dlopen`), so a binary built with NVENC still links and starts on a
machine with no NVIDIA driver, falls through to the next encoder, and ends at the
statically-linked openh264 software encoder. No public type, function, or error
variant names a backend, so swapping one is never a breaking change for you.

## Installation

```bash
cargo add moq-video
```

Hardware codecs are on by default. Rendering and PipeWire screen capture are not,
since they pull in a graphics stack and a `libpipewire` build dependency
respectively:

```bash
cargo add moq-video --features render,pipewire
```

| Feature | Default | Pulls in |
| --- | --- | --- |
| `nvenc` / `nvdec` | yes | NVIDIA encode/decode on Linux (`cudarc`, `moq-nvenc`) |
| `vaapi` | yes | Intel/AMD encode on Linux (`moq-vaapi`) |
| `render` | no | `wgpu` and the GPU renderer |
| `pipewire` | no | Wayland/X11 screen capture via xdg-desktop-portal |

The three default features are Linux-only in effect (their dependencies are), and
`--no-default-features` gives a slim build that still captures V4L2 and encodes
with openh264. A relay never needs any of them.

## Publishing

`encode::publish_capture` is the turnkey path: it advertises the track and catalog
up front, then opens the camera only while somebody is watching and releases it
when the last subscriber leaves.

```rust
use moq_video::{capture, encode};

// Defaults to the default camera; pick a specific one by id.
let mut config = capture::Config::default();
if let Some(camera) = capture::cameras().await?.first() {
    config.source = camera.source();
}

encode::publish_capture(
    broadcast,
    catalog,
    config,
    encode::Options::default(),
    clock,
).await?;
```

To encode frames you produced yourself (from a decoder, a game engine, or your own
pixels), drive `encode::Encoder` and publish the results with `encode::Producer`.
Keyframes are the encoder's business: it inserts them per `encode::Config::gop`,
and `Encoder::keyframe` is there for the rarer case where you need one at a
specific frame.

## Subscribing

`decode::Consumer` is the mirror. It reads the rendition's catalog entry to pick a
decoder, then hands back one `Frame` per call:

```rust
use moq_video::decode;

// `rendition` is the hang catalog's VideoConfig for the track you want.
let mut video = decode::Consumer::new(&broadcast, &rendition, "video", decode::Config::default()).await?;

while let Some(frame) = video.read().await? {
    // frame.timestamp, frame.surface
}
```

## Zero-copy

A hardware-decoded frame stays on the GPU. `Surface` is a `#[non_exhaustive]` enum
naming what actually holds the pixels: a `CVPixelBuffer` on macOS, a Direct3D 11
texture on Windows, CUDA memory on Linux, or plain I420 anywhere.

That matters for two paths that would otherwise pay for a round trip through
system memory on every frame:

- **Transcode.** A decoded NVDEC frame feeds NVENC without leaving the GPU, and
  `decode::Config::resize` scales it there too.
- **Playback.** The `render` module imports the decoder's surface as a texture
  rather than copying it, converting YUV to RGB in a shader.

Matching on `Surface` stays portable because every variant has a universal
fallback in `Surface::into_i420()`: take the fast path you recognize and let the
`_` arm download. The renderer does exactly this, and an import path that keeps
failing retires itself after a few frames instead of paying for the attempt
forever.

## Rendering

`render::Renderer` takes a `wgpu` device and queue and hands back a
`wgpu::Texture` per frame. That texture is the entire integration seam: present
it to a window, feed it to egui or bevy, or copy it back. The module carries no
windowing or UI dependency and never picks a surface format for you.

```rust
use moq_video::render::{Config, Renderer};

let mut renderer = Renderer::new(&device, &queue, Config::new())?;

while let Some(frame) = video.read().await? {
    let texture = renderer.render(&frame)?;
    // ... present it ...
}
```

The `wgpu` version this was built against is re-exported as
`moq_video::render::wgpu`, so you name the exact version rather than guessing at
a compatible one.

Color is carried end to end rather than assumed: `Color` names the matrix and
range (BT.601 or BT.709, limited or full), capture labels what it produced, and
the shader converts accordingly.

## Devices

Each enumerator returns ids that go straight back into `capture::Config::source`:

```rust
moq_video::capture::cameras().await?;   // webcams
moq_video::capture::displays().await?;  // monitors
moq_video::capture::windows().await?;   // single windows (macOS)
moq_video::capture::apps().await?;      // every window of an app (macOS)
```

The [`moq devices`](/bin/cli) subcommand prints the same lists from the command
line.

## API Reference

Full API documentation: [docs.rs/moq-video](https://docs.rs/moq-video)

## Next Steps

- Pair it with [moq-audio](/lib/rs/crate/moq-audio) for the other half of a call
- Publish through [hang](/lib/rs/crate/hang) catalogs and [moq-mux](/lib/rs/crate/moq-mux) containers
- Capture and publish from the command line with [moq-cli](/bin/cli)

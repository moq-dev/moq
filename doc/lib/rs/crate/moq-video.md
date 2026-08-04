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
application has no such thing, so `moq-video` provides it: no ffmpeg, no
GStreamer, no system codec to install. Just the platform APIs, plus a vendored
statically-linked openh264 so a build never depends on what the host happens to
have.

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

Backend selection is automatic and ordered: platform hardware first, then the
software fallback. The Linux hardware libraries are loaded at runtime (`dlopen`),
so a binary built with NVENC still links on a GPU-less builder and starts on a
machine with no NVIDIA driver, falling through to the next backend instead of
failing to load. No public type, function, or error variant names a backend, so
swapping one is never a breaking change for you.

**openh264 is the fallback for H.264 only.** It is statically linked and always
compiled in, so H.264 encodes and decodes on any machine. H.265 is hardware-only:
with no usable platform backend you get `NoEncoder` / `NoDecoder` rather than a
slow path. AV1 is decode-only, via NVDEC.

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

`encode::Sink` is the same encoder with a thread of its own, and an `async` API on
top. Reach for it when the codec outlives a single thread's stack: an object
shared between threads, a handle behind an FFI boundary, or a task that migrates
between executor workers. Hardware codecs are not all thread-agnostic (a Media
Foundation MFT's COM apartment is per-thread, so building it on one thread and
dropping it on another corrupts COM state), and the sink confines the whole
encoder lifetime to one thread so callers do not have to. Awaiting rather than
blocking is the point: the executor keeps its worker while a slow hardware encoder
works through a frame. A plain `Encoder` you build, drive, and drop inside one
function needs none of this.

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

`Surface` is a `#[non_exhaustive]` enum naming what actually holds a frame's
pixels: a `CVPixelBuffer` on macOS, a Direct3D 11 texture on Windows, CUDA memory
on Linux, or plain I420 anywhere. Keeping a decoded frame in the first three
avoids a round trip through system memory on every frame.

How far that gets today depends on the platform, so here is the honest matrix
rather than a blanket promise:

| Platform | Decode output | Zero-copy transcode | Zero-copy render |
| --- | --- | --- | --- |
| macOS | `PixelBuffer` (VideoToolbox) | yes | yes, via `CVMetalTextureCache` |
| Linux | `Cuda` (NVDEC) | yes, straight into NVENC | no, downloaded to I420 first |
| Windows | `Texture` (Media Foundation / DXVA) | yes, through the Direct3D11 video processor | no, downloaded to I420 first |

`Frame::resize` stays on the GPU through a `VTPixelTransferSession`, CUDA kernel,
or Direct3D11 video processor. Call `Frame::resize_with` with
`resize::Acceleration::Cpu` to force a download and CPU resize. A driver that
rejects GPU resizing returns to CPU scaling and warns once. Rendering is
zero-copy on macOS only; the Vulkan and EGL importers that would extend it are
tracked in [#2481](https://github.com/moq-dev/moq/issues/2481).

Matching on `Surface` stays portable because every variant has a universal
fallback in `Surface::into_i420()`: take the fast path you recognize and let the
`_` arm download. The renderer does exactly this, and an import path that keeps
failing retires itself after a few frames instead of paying for the attempt
forever. Set `render::Config::zero_copy` to `false` to force the download path
when comparing output or working around a driver.

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

`Color` names the matrix and range (BT.601 or BT.709, limited or full), and the
shader converts per frame rather than assuming one space. A capture labels what it
produced, so a locally captured frame renders correctly with no help from you.

A decoded frame is the case to watch. The authoritative answer lives in the
bitstream's VUI and does not survive decoding, and a Windows or CUDA surface
carries no color metadata at all, so the renderer falls back to inferring from
resolution (BT.601 at 576 lines or fewer, BT.709 above). That is a good guess, not
a correct one: full-range content, or an unusual resolution, will render with the
wrong range or matrix. Set `render::Config::color` when you know the stream's
color space and the frame does not.

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

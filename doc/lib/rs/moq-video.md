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
| `decode` | A subscribed track back to frames | VideoToolbox, Media Foundation/DXVA, NVDEC, VAAPI, V4L2 M2M, MediaCodec (Android), openh264 |
| `render` | A frame as a `wgpu` texture | wgpu, with zero-copy Metal and Vulkan imports |

Highlights:

- **Automatic backend selection**, hardware first. Linux GPU libraries are `dlopen`ed at runtime, so one binary starts anywhere and warns when it falls back to software. openh264 (the default-on `openh264` feature) is statically linked as the H.264 fallback; H.265 is hardware-only; AV1 decodes via NVDEC. The VAAPI encoder, decoder, and GPU resize share one render node: the first whose driver does all three, or the one the `MOQ_VAAPI_DEVICE` environment variable names (for example `/dev/dri/renderD129`).
- **Publish on demand.** `encode::publish_capture` advertises the track up front and opens the camera only while someone subscribes.
- **GPU ownership where the platform allows.** Matching codec backends consume their native GPU surfaces directly. The renderer imports `CVPixelBuffer` and supported DMA-BUF formats. Linux/NVIDIA producers can import dedicated Vulkan RGBA8 slots into CUDA with timeline-semaphore ordering and completion-driven slot return. Vulkan/CUDA surfaces deliberately have no CPU pixel fallback; other surfaces use the typed `Surface::into_i420()` and configured `Surface::to_rgba(config)` when needed.
- **Live bitrate control** where the selected backend supports it, without forcing a keyframe. An unsupported backend keeps its opening rate.
- **Latency presets.** `encode::Config::preset` picks `Preset::LowLatency` (the default), `Balanced`, or `Quality`, with bitrate set separately. `Encoder::applied()` and `Sink::applied()` report the preset whose controls actually took effect and names them for display. See [Encoder presets](#encoder-presets).
- **Typed group structure.** `encode::Config::gop` is a `Gop` enum (`Keyframe { interval }` today), so a later mode adds a variant instead of replacing the field. `cut()` opens a group at the next frame on both `Encoder` and `Sink`, and refuses with `Error::CutUnsupported` on a backend that cannot force one rather than letting the boundary silently slip to the interval.
- **Device enumeration** for cameras, displays, windows, and apps, matching `moq devices`.

With `capture` enabled, `capture::camera_modes` lists a Linux camera's convertible
sizes and exact rates before configuring it. Rates are `moq_video::Rate`, an
exact rational that preserves 30000/1001: `frames(duration)` counts frames,
`rounded()` gives the nearest whole frame rate for integer-only platform APIs,
and `as_f64()` yields the catalog value.
Sizes and rates shared by YUYV and MJPEG are combined; invalid I420 dimensions
are excluded. A size range contributes its smallest and largest valid sizes aligned to the
driver's step,
and an empty rate list means no discrete intervals were reported. Device errors
are returned rather than treated as an empty list. Other platforms return
`Error::Unsupported`.

With `pipewire` enabled, `capture::cameras` also lists PipeWire camera nodes as
`pipewire:<node name>` after the V4L2 devices, and `pipewire` alone opens the
camera with the highest session priority, the session manager's default. That
reaches cameras V4L2 cannot: a Raspberry Pi CSI camera behind libcamera, and any
camera from inside a Flatpak or Snap sandbox, where the default camera comes
through the xdg-desktop-portal Camera interface. The mode is chosen from the
node's own format list by the same rules as V4L2 (below) and offered exactly,
across YUY2, NV12, RGB, and MJPEG, and `camera_modes` lists the same modes.

`capture::Config::framerate` is an `Option<Rate>` request in the same exact
type; the stream reports the rate the device accepted, or `None` when the
driver reported none.
V4L2 and PipeWire cameras choose the closest geometry, then the format whose
accepted rate is nearest the request, then the cheaper conversion when both
match equally well.

```rust
let mut video = moq_video::decode::Consumer::new(&broadcast, &rendition, "video", Default::default()).await?;
let mut renderer = moq_video::render::Renderer::new(&device, &queue, Default::default())?;
while let Some(frame) = video.read().await? {
    let texture = renderer.render(&frame)?;
}
```

```bash
cargo add moq-video                      # nvidia, mediacodec, openh264 on by default
cargo add moq-video --features capture   # camera + screen capture, no system build deps
cargo add moq-video --features render    # wgpu rendering
cargo add moq-video --features v4l2      # Linux V4L2 M2M codecs, no system build deps
cargo add moq-video --features vaapi     # Linux VAAPI codecs (bindgen needs libclang)
cargo add moq-video --features pipewire  # Wayland screen + PipeWire cameras (links libpipewire)
cargo add moq-video --no-default-features --features openh264  # software H.264 only
cargo add moq-video --no-default-features --features nvidia    # Linux NVIDIA only, no C++ or wgpu
```

The language bindings use the codec-only shape, which is what keeps their
Android floor at API 24 instead of MediaCodec's API 26 entry points.

API: [docs.rs/moq-video](https://docs.rs/moq-video). Pair with
[`moq-audio`](/lib/rs/moq-audio).

`decode::Config::output` chooses where decoded pictures live. `Output::Native`,
the default, hands back whatever the backend decoded into: a `CVPixelBuffer`, a
Direct3D11 texture, a CUDA buffer, a VAAPI DMA-BUF for zero-copy rendering, or
CPU pixels from a software decoder. `Output::Cpu` delivers every picture as
`Surface::I420`, decoded straight to system memory where the backend can.
Native surfaces still answer `Surface::into_i420()`, which returns an `I420`
with geometry and color metadata intact; call `I420::into_data()` only when
packed bytes are required. `decode::Config::scale_hint` is best effort and only
a decoder with a hardware scaler honors it; `Frame::resize` is the exact-size
operation. `decode::Consumer` takes `decode::Options`, which carries the
subscription's `start` and `max_age` beside the decoder config.

Linux/NVIDIA applications with a native Vulkan producer use
`frame::vulkan::Importer`. Each reusable image is a dedicated, optimal-tiling
`VK_FORMAT_R8G8B8A8_UNORM` allocation exported with an opaque memory FD, plus an
opaque-FD timeline semaphore and the physical-device UUID. Publishing consumes
the producer-owned slot; awaiting its completion returns that slot only after
CUDA readers finish. Import capacity bounds retained images. Unsupported
devices, formats, layouts, and synchronization are errors, with no CPU mapping
or staging fallback. A non-exportable application image needs one Vulkan GPU
copy into an exportable slot. The image is `VK_FORMAT_B8G8R8A8_UNORM` when
imported through `Image::bgra8` instead.

`frame::cuda::Converter` turns a published Vulkan frame into the NV12
`Surface::Cuda` NVENC encodes in place, on the GPU, in one declared color space
(matrix and range) with 4:2:0 chroma averaged per 2x2 block and no transfer
function applied. Its buffers come from a pool sized at construction, and
`cuda::Frame::resize` scales a converted frame for a smaller rendition from the
same pool, so one captured frame feeding HD and SD holds a fixed number of
buffers and a producer that outruns its encoder gets an error instead of
unbounded device memory. Open the encoder with `encode::Kind::Named("nvenc")`
and the same `encode::Config::color`: `Kind::Auto` could fall back to a software
encoder that reads the frame back, and the portable `Surface::resize` downloads
when the GPU scaler fails. Everything under `frame::cuda` and `frame::vulkan`
runs on the device or returns an error.

## Encoder presets

A preset trades per-frame encode time for compression at the configured
bitrate. None reorders frames, and none describes keyframe join time, transport
delay, or viewer playout. Each backend maps a preset onto the controls it has,
and reports what it applied rather than echoing the request:

| Backend | Low latency | Balanced | Quality |
| --- | --- | --- | --- |
| NVENC | P1 | P4 | P7 |
| openh264 | low complexity | medium complexity | medium complexity, reported as Balanced |
| VAAPI | IDR/P only, one frame in flight | same, reported as Low latency | same, reported as Low latency |
| VideoToolbox | real-time, no reordering | same, reported as Low latency | same, reported as Low latency |
| Media Foundation, MediaCodec, V4L2 | unconfirmed, no preset reported | unconfirmed | unconfirmed |

NVENC always uses low-latency tuning, CBR, and a one-frame VBV. Only NVENC and
openh264 were measured. VAAPI and VideoToolbox report the one set of controls
their code applies. Media Foundation and MediaCodec only request low latency,
and V4L2 leaves frame reordering to the driver, so they report no preset until
someone measures that hardware.

Measured on an RTX 3070 Ti and a 32-thread x86 host under load from other jobs,
encoding Big Buck Bunny at 720p30 and 1.5 Mbps. Frame-to-packet is the time
from `encode` to the packet stamped with that frame, including the CPU upload.
Encode is NVENC's submit and output lock alone, timed with temporary
instrumentation. Quality is PSNR and SSIM against the source.

| Backend | Preset | Frame-to-packet p50 | Encode p50 | PSNR | SSIM |
| --- | --- | --- | --- | --- | --- |
| NVENC H.264 | Low latency | 4.6 ms | 1.6 ms | 39.19 dB | 0.9787 |
| NVENC H.264 | Balanced | 5.7 ms | 2.1 ms | 39.22 dB | 0.9789 |
| NVENC H.264 | Quality | 6.3 ms | 3.3 ms | 39.26 dB | 0.9792 |
| NVENC H.265 | Low latency | 5.7 to 8.0 ms | 1.9 to 2.2 ms | 39.62 dB | 0.9809 |
| NVENC H.265 | Balanced | 7.2 to 8.2 ms | 2.9 to 3.3 ms | 39.72 dB | 0.9815 |
| NVENC H.265 | Quality | 7.6 to 8.1 ms | 3.1 to 3.4 ms | 39.73 dB | 0.9816 |
| openh264 | Low latency | 10.6 ms | | 39.87 dB | 0.9818 |
| openh264 | Balanced | 12.2 ms | | 39.92 dB | 0.9823 |

Ranges span two runs. About half of NVENC's frame-to-packet time is allocating
input and output buffers per frame, the same cost for every preset.

At 1080p30 and 4 Mbps on synthetic moving content, NVENC H.264 encode rises from
3.2 ms (P1) to 3.8 ms (P4) and 6.7 ms (P7), and openh264 frame-to-packet from
20.8 to 22.4 ms. No backend measured here held a frame: each packet came back
from the call that submitted it. openh264 skips frames to hold its rate (11 of
300 at 720p). p95 frame-to-packet ranged 6 to 25 ms and tracked host load, not
the preset. Other backends need their own run:
`cargo run --release -p moq-video --example encode-presets` reports
frame-to-packet latency, throughput, CPU time, held and skipped frames, and
bitrate per preset, and writes each stream for scoring with ffmpeg.

An `Encoder` or `Sink` takes one frame per call and queues none of its own. A
source that outruns the codec should drop raw frames it has not submitted,
never encoded packets.

`just rs vulkan-cuda` runs the opt-in native Vulkan/CUDA/NVENC hardware
exercise, including a three-view 1280x720 workload that reports per-stage
latency and CPU time.

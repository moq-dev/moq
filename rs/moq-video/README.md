# moq-video

Native video capture, encoding, decoding, and publishing for
[Media over QUIC](https://github.com/moq-dev/moq).

The video counterpart to [`moq-audio`](https://crates.io/crates/moq-audio).
Everything is native per-platform code with no ffmpeg dependency: capture, color
conversion, and the codec backends are all in-tree or thin wrappers over system
frameworks / vendored static libs. The public API is codec-agnostic, so no
signature, type, or error variant names a backend or a capture implementation;
swapping or bumping a backend crate is not a breaking change.

## Capture

The opt-in `capture` feature exposes the device APIs and their per-platform
backends. Enable it with `cargo add moq-video --features capture`; the default
codec-only build accepts frames supplied by the caller without pulling Linux
V4L2 and libclang build dependencies.

Per-platform, picked at compile time:

- **macOS**: AVFoundation (camera) and ScreenCaptureKit (display, window, or
  application), yielding zero-copy `CVPixelBuffer` surfaces straight to
  VideoToolbox.
- **Linux**: native V4L2 (camera; YUYV resampled, MJPEG via `zune-jpeg`) and
  xdg-desktop-portal + PipeWire on Wayland (display; behind the `pipewire`
  feature), with native X11 monitor/window selection and capture as the X11
  fallback. The Wayland picker dialog chooses the screen, and the portal's
  restore token is reused so demand-driven reopens don't re-prompt.
- **Windows**: native Media Foundation (camera; `IMFSourceReader`) and DXGI
  Desktop Duplication (display), plus GDI single-window capture. Both convert
  BGRA to CPU I420 and use the ids returned by the enumerators.

`capture::cameras()` lists AVFoundation, V4L2, or Media Foundation cameras with
identifiers accepted by `capture::Source::Camera`. `capture::displays()` does
the same for macOS, Windows, and X11 displays. `capture::windows()` lists macOS,
Windows, and X11 windows. Wayland display selection stays in the desktop portal
picker, which does not expose a stable display identifier.

Embedded applications can consume raw capture without creating a MoQ
broadcast:

```rust
let mut config = moq_video::capture::Config::default();
config.source = moq_video::capture::Source::Display(None);

let mut capture = moq_video::capture::open(&config).await?;
while let Some(frame) = capture.read().await? {
    // Encode, render, or inspect the newest captured frame; `frame.surface`
    // holds the pixels and `frame.timestamp` the capture time.
}
```

The stream retains only the newest unconsumed frame, so a slow encoder adds
drops rather than latency. `read` ends with `None` when the source stopped for
a benign reason, such as a window resize, so reopen to follow it. Permission
denial and a source disappearing are terminal, reported as
`Error::PermissionDenied` and `Error::SourceUnavailable`.

## Encode

The codec is chosen via `encode::Codec`. Backends are tried in order (hardware
first, then software) and the first that opens wins; `encode::Kind` narrows the
choice (`Auto` / `Hardware` / `Software` / a named backend).

| Codec | Software | macOS | Windows | Linux | Android |
|---|---|---|---|---|---|
| H.264 | OpenH264 (feature `openh264`, default) | VideoToolbox | Media Foundation | NVENC (feature `nvidia`), VAAPI (feature `vaapi`) | MediaCodec (feature `mediacodec`, API 26+) |
| H.265 | none | VideoToolbox | Media Foundation | NVENC (feature `nvidia`) | MediaCodec (feature `mediacodec`, API 26+) |

Every backend emits Annex-B with in-band parameter sets (SPS/PPS, plus VPS for
H.265), so the matching `moq_mux::codec` importer handles framing and catalog
registration directly. There is no software H.265 encoder (it's hardware-only).

`encode::Encoder::encode` takes a raw `Frame` (a timestamp plus a `Surface`
holding the pixels) and returns `encode::Encoded`s: one whole access unit each,
carrying the timestamp of the picture it was encoded from. That matters for a
backend that buffers, which hands back an earlier frame's access unit while a
later one goes in, and for the tail `finish()` drains. Bring your own pixels with
`Surface::rgba(...)`, or feed a frame straight from capture or `decode`.

Group boundaries are automatic: `Config::gop` says how the stream is divided
(`Gop::Keyframe { interval }` places a keyframe every so many frames, and an
interval of zero is refused at open), so an application never has to think
about them. `Encoder::cut()` opens a group at the next frame when something
outside the encoder needs a decodable starting point there: a source group
boundary, a scene change, a source switch. The request is held until a frame
arrives, so it is safe to call before you have one. It fails with
`Error::CutUnsupported` on a backend that cannot force a boundary (a V4L2
driver without the force-keyframe control), and queues nothing then: groups
keep falling where `Config::gop` puts them. `encode::Sink` answers the same
way, awaited.

Two public entry points:

- `encode::publish_capture(...)` captures a webcam, encodes it, and publishes on
  demand: the track and catalog are advertised up front, but the camera opens
  only while a subscriber is watching and is released when the last one leaves.
- `encode::Producer` publishes frames you encoded yourself (`publish(&[Encoded])`),
  handling the catalog and framing. Each is published at its own timestamp.

The default features are `openh264`, `nvidia`, and `mediacodec`. OpenH264 keeps
a working software H.264 fallback but compiles vendored C++; disable defaults
and select native features to omit it. `nvidia` is Linux-only, `dlopen`s the
driver at runtime, and needs no build-time toolkit. `vaapi` and `v4l2` are
opt-in because their bindgen needs libclang on the build host (plus kernel
headers for `v4l2`). `render` is also opt-in so codec-only consumers do not
compile wgpu.

### Vulkan producers on NVIDIA

`frame::vulkan::Importer` accepts dedicated optimal-tiling
`VK_FORMAT_R8G8B8A8_UNORM` (`Image::rgba8`) or `VK_FORMAT_B8G8R8A8_UNORM`
(`Image::bgra8`) images exported as opaque memory FDs. The producer
also exports a timeline semaphore and supplies the Vulkan physical-device UUID;
imports with another CUDA device, format, layout, allocation shape, or sync
mechanism are refused. Vulkan signals `Timeline::ready` after writes and the
transition to `VK_IMAGE_LAYOUT_GENERAL`. CUDA waits on that value and signals
`Timeline::complete` after all readers queued on the frame stream.

An imported `Slot<T>` owns the producer's `T`. `Slot::publish` consumes it and
`Completion::wait` returns the same slot only after CUDA completion, so a pool
cannot overwrite an in-flight image. Importer capacity bounds retained slots,
and a dedicated worker drains completion after capture stops or a receiver is
cancelled without blocking the producer thread. If the original application
image is not exportable, copy it on Vulkan into a dedicated exportable slot;
that is one GPU image copy, not zero-copy. There is no CPU mapping, download, or
staging fallback for `Surface::Vulkan`.

`frame::cuda::Converter` turns a published Vulkan frame into the NV12
`Surface::Cuda` that NVENC encodes in place. It runs on the GPU in one declared
color space (matrix and range), averages 4:2:0 chroma per 2x2 block, applies no
transfer function, and draws every buffer from a pool sized at construction;
`cuda::Frame::resize` scales a converted frame for a smaller rendition from the
same pool. One captured frame feeding HD and SD therefore holds a fixed number
of buffers, and a producer that outruns its encoder gets an error rather than
unbounded device memory. Open the encoder with `encode::Kind::Named("nvenc")`
and the same `encode::Config::color`: `Kind::Auto` could fall back to a software
encoder that reads the frame back, and the portable `Surface::resize` downloads
when the GPU scaler fails. Everything under `frame::cuda` and `frame::vulkan`
runs on the device or returns an error.

Run `just rs vulkan-cuda` for the opt-in native hardware exercise. It creates a
Vulkan image independently of Unreal, imports it once into CUDA, checks repeated
slot reuse and held-reader ordering, and tears down through cancellation; a
second test converts RGBA and BGRA uploads to NV12, scales them, fills the pool,
and encodes both renditions through NVENC. A third runs the target workload,
three 1280x720 views at 30 fps, and prints per-stage latency and CPU time
instead of asserting a threshold.

## Decode

`decode::Consumer` (the mirror of `moq_audio::decode::Consumer`) subscribes to an
H.264, H.265, or AV1 track and returns raw `Frame`s. A hardware-decoded frame stays
on the GPU: feeding it back to a compatible hardware `encode::Encoder` on the
same device keeps it there (the transcode path), while `into_i420()` downloads
it. An encoder that can't take that surface (openh264, or a different device)
downloads it through I420 for you. Every frame carries a `Surface`, a
`#[non_exhaustive]` enum naming where the pixels live (`PixelBuffer` on macOS,
`Texture` on Windows, `Vulkan` and `Cuda` on Linux, `HardwareBuffer` on Android,
or CPU `I420`). Match
it to take a GPU path for a representation you recognize, and fall back to
`Surface::into_i420()` for readback-capable surfaces. GPU-only
`Surface::Vulkan` refuses CPU conversion. On macOS `Surface::into_pixel_buffer()`
is the mirror: free for a hardware-decoded frame, an upload for a CPU one.
`Surface::into_i420()` returns typed pixels with size and color intact;
`I420::into_data()` explicitly extracts the packed bytes. `Surface::to_rgba(config)`
and `Surface::to_bgra(config)` are the portable exits for CPU
image and UI toolkits, returning owned, tightly packed pixels with the surface's
color metadata applied. Both orders are there because toolkits disagree and the
conversion is a full pass over the frame: producing the order the caller wants
costs nothing extra, while producing the other one and swapping two channels
afterwards costs a second pass. They borrow the surface, so a frame held behind
an `Arc` shared with something else converts without being unwrapped first.
Backends are tried hardware-first, like encode:

| Codec | Software | macOS | Windows | Linux | Android |
|---|---|---|---|---|---|
| H.264 | OpenH264 (feature `openh264`, default) | VideoToolbox | Media Foundation (DXVA) | NVDEC (feature `nvidia`), VAAPI (feature `vaapi`) | MediaCodec (feature `mediacodec`, API 26+) |
| H.265 | none | VideoToolbox | Media Foundation (DXVA) | NVDEC (feature `nvidia`) | MediaCodec (feature `mediacodec`, API 26+) |
| AV1 | none | none | none | NVDEC (feature `nvidia`) | MediaCodec (feature `mediacodec`, when the device provides it) |

On macOS VideoToolbox decodes H.264 and H.265 on hardware, pulling the parameter
sets (SPS/PPS, plus VPS for H.265) out of each keyframe to build the format
description. On Windows the Microsoft decoder MFT runs synchronously with a
Direct3D11 device bound to it, so the decode happens on the GPU through DXVA
(NVDEC / Intel / AMD). H.264 falls back to openh264 on a GPU-less host; H.265 has
no software decoder, so it needs the GPU path (on Windows, an HEVC decoder MFT:
the inbox HEVC Video Extensions or a vendor one). On Linux, NVDEC decodes H.264,
H.265, and 8-bit 4:2:0 AV1 to CUDA NV12 frames; AV1 is decode-only and is useful
for AV1 source to H.264/H.265 transcode rungs. VAAPI decodes H.264 to DMA-BUF
surfaces the renderer imports without a download. A non-H.264/H.265/AV1
rendition yields `Error::UnsupportedCodec`.

`decode::Config::output` says where decoded pictures live: `Output::Native`
(the default) hands back whatever the backend decoded into, a GPU surface or
CPU pixels, and `Output::Cpu` delivers every picture as `Surface::I420`,
decoded straight to system memory where the backend can and downloaded where it
cannot. `decode::Config::scale_hint` asks a decoder with a hardware scaler
(NVDEC) to emit that size; it is a hint, so check `Frame::size` and use
`Frame::resize` for the exact size. `decode::Consumer` takes `decode::Options`,
which pairs that config with the subscription's `start` and `max_age`.

Common feature sets:

```bash
cargo add moq-video                                      # native defaults + OpenH264, no renderer
cargo add moq-video --no-default-features --features openh264  # software H.264 only
cargo add moq-video --no-default-features --features nvidia    # Linux NVIDIA only
cargo add moq-video --features render                    # add the wgpu renderer
```

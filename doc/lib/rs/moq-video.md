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
| `capture` | Camera, display, window, or application frames | AVFoundation + ScreenCaptureKit (macOS), V4L2 + X11/portal + PipeWire (Linux), Media Foundation + Windows.Graphics.Capture (Windows) |
| `encode` | Frames to H.264/H.265, published as a hang track | VideoToolbox, Media Foundation, NVENC, VAAPI, V4L2 M2M, MediaCodec (Android), openh264 |
| `decode` | A subscribed track back to frames | VideoToolbox, Media Foundation/DXVA, NVDEC, VAAPI, V4L2 M2M, MediaCodec (Android), openh264 |
| `render` | A frame as a `wgpu` texture | wgpu, with zero-copy Metal and Vulkan imports |

Highlights:

- **Automatic backend selection**, hardware first. Linux GPU libraries are `dlopen`ed at runtime, so one binary starts anywhere and warns when it falls back to software. openh264 (the default-on `openh264` feature) is statically linked as the H.264 fallback; H.265 is hardware-only; AV1 decodes via NVDEC. The VAAPI encoder, decoder, and GPU resize share one render node: the first whose driver does all three, or the one the `MOQ_VAAPI_DEVICE` environment variable names (for example `/dev/dri/renderD129`).
- **Publish on demand.** `encode::publish_capture` advertises the track up front and opens the camera only while someone subscribes. `encode::Control::new` is the same with a handle kept, the mirror of `moq-audio`'s: `Control::cut()` asks for a keyframe, requests coalesce, any keyframe serves them (the GOP cadence included), and a forced one lands at least 500ms after any other. On a backend that cannot force one, `cut()` returns `Error::CutUnsupported` once the startup probe or a reopen has found that out, and the publish carries on at its cadence. Frames are stamped on the catalog's clock, and dropping the last `Control` ends the driver promptly, even mid-open.
- **GPU ownership where the platform allows.** Matching codec backends consume their native GPU surfaces directly. The renderer imports `CVPixelBuffer` and supported DMA-BUF formats. Linux/NVIDIA producers can import dedicated Vulkan RGBA8 slots into CUDA with timeline-semaphore ordering and completion-driven slot return. Vulkan/CUDA surfaces deliberately have no CPU pixel fallback; other surfaces use the typed `Surface::into_i420()` and configured `Surface::to_rgba(config)` when needed.
- **Live bitrate control** where the selected backend supports it, without forcing a keyframe. An unsupported backend keeps its opening rate.
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

Windows display and window capture require Windows 10 2004 (build 19041) or
newer and use Windows.Graphics.Capture without a Desktop Duplication or GDI
fallback. `Config::cursor` controls cursor capture. Frames leave the capture
pool as owned GPU NV12 textures; odd native edges are cropped to even sizes.
The conversion declares BT.601 limited range through 576 lines and BT.709
limited range above it, preserving that color through resize and CPU download.
Media Foundation encodes on the capture device; openh264 uses the download path.
Display enumeration covers all adapters, and window enumeration excludes
DWM-cloaked windows and reports visible frame bounds. Windows `display:N`
selectors are enumeration indices, not persistent monitor identities. Switching
from Desktop Duplication to WGC can change their mapping; enumerate displays
again and reselect the intended monitor after upgrading. Enumeration does not
start capture. A settled source resize ends the stream so callers can reopen;
closing the captured item returns `Error::SourceUnavailable`. Dropping the
stream releases its session even when no new frames arrive.
If no usable first frame arrives within five seconds after capture starts,
opening fails with `Error::SourceUnavailable` and releases the session.

Windows shows a capture border by default. On build 20348 or newer the backend
requests borderless access; denial keeps the border and does not fail capture.
Older supported builds keep the border. Application capture and system audio
are not provided by this backend.

X11 display and window capture use MIT-SHM 1.2 fd-backed buffers on local Unix
connections. Remote connections and servers without that support use
`GetImage`. Shared-memory setup or read failures are reported rather than
silently switching paths. RandR events refresh monitor geometry; window
`ConfigureNotify` events update the captured size. A settled size or monitor
change ends the stream so callers can reopen. Unmapped windows hold capture
until viewable again. RGB conversion reuses its buffer.

On Linux, `just rs x11-bench` compares the production SHM and `GetImage`
capture paths on the current local X display, which must be at least 1920×1080
and support MIT-SHM 1.2. It checks pixels for static and changing images at
three sizes and records paired median frame times and their ratio. Timings
include RGB and I420 conversion; capture pacing and fixture drawing are not
measured. The GetImage baseline keeps the same event handling and buffer reuse,
so it isolates transport rather than comparing the entire pre-quest backend.
`just rs x11-rgb-bench` isolates conversion-buffer allocation.

`just rs x11-bench-ci` runs both workloads with an isolated Xvfb server and
writes `.scratch/x11-capture-benchmark.log` (override with `MOQ_X11_BENCH_LOG`).
PR and nightly CI retain the log for 30 days, including the source commit,
paired GetImage baseline and SHM/baseline ratios. Compare those
records across runs to investigate regressions; timing has no hard threshold
until runner variance is measured. Missing SHM support or incorrect pixels
fails the test. Xvfb coverage does not replace real-desktop capture, lifecycle
and before/after performance checks.

With `pipewire` enabled, `capture::cameras` also lists PipeWire camera nodes as
`pipewire:<node name>` after the V4L2 devices. V4L2 lists only devices offering
YUYV or MJPEG. A PipeWire V4L2 node is hidden only when its device path was
already listed by V4L2, so identical webcams stay distinct, and PipeWire-only
cameras and NV12- or RGB-only V4L2 devices remain visible through PipeWire. Explicit `pipewire:<node name>`
selectors still open hidden nodes, and `pipewire` alone opens the
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
function applied. Its buffers come from a pool sized at construction:
`Converter::reserve` holds one as a `cuda::Slot`, which `Slot::convert` fills
with the captured frame or `Slot::resize` with a smaller rendition of it. One
captured frame feeding HD and SD holds a fixed number of buffers, and a producer
that outruns its encoder gets `None` from `reserve`, its cue to drop the frame,
instead of unbounded device memory. A slot dropped unfilled, or consumed by a
failed conversion, returns its buffer, so only a real failure is an error. Open the encoder with `encode::Kind::Named("nvenc")`
and the same `encode::Config::color`: `Kind::Auto` could fall back to a software
encoder that reads the frame back, and the portable `Surface::resize` downloads
when the GPU scaler fails. Everything under `frame::cuda` and `frame::vulkan`
runs on the device or returns an error.

`just rs vulkan-cuda` runs the opt-in native Vulkan/CUDA/NVENC hardware
exercise, including a three-view 1280x720 workload that reports per-stage
latency and CPU time.

## Intel QuickSync on Linux

Enable the `vaapi` feature to use Intel QuickSync through VA-API. It is off by
default because building `moq-vaapi` needs libclang. The backend currently
encodes and decodes 8-bit H.264; HEVC and AV1 support in the Intel driver does
not make those codecs available through this backend yet. No oneVPL or Media
SDK installation is needed.

At runtime, install libva and the Intel `iHD` media driver, and make sure your
user can open the GPU's `/dev/dri/renderD*` node. On Ubuntu:

```bash
sudo apt install intel-media-va-driver-non-free vainfo
vainfo --display drm --device /dev/dri/renderD128
```

Use the render node present on your machine. `vainfo` should report the Intel
iHD driver with H.264 `VAEntrypointVLD`, `VAEntrypointEncSlice` or
`VAEntrypointEncSliceLP`, and `VAEntrypointVideoProc`. Intel's
[media driver documentation](https://github.com/intel/media-driver) lists
supported GPUs and the differences between the full-feature and free-kernel
packages.

Automatic encoder and decoder selection uses VAAPI when it opens successfully,
otherwise H.264 can fall back to openh264. Set `encode::Kind::Named("vaapi".into())`
and `decode::Kind::Named("vaapi".into())` to require this backend and report an
unavailable driver instead. `MOQ_VAAPI_DEVICE` selects a render node when
multiple GPUs are present.

Verified on Intel Core Ultra 7 270K Plus / Arrow Lake with iHD 26.2.4: CPU
uploads, hardware decode, DMA-BUF re-encoding, GPU resize, live bitrate changes,
color metadata, and Vulkan rendering with decoder surface reuse. To run the
same checks from this repository:

```bash
just rs test -p moq-video --features vaapi,render --no-capture
just rs test -p moq-video --features vaapi,render --run-ignored only --no-capture \
  -E 'test(nv12_dmabuf) | test(i420_dmabuf) | test(decoded_frames_reach) | test(recycled_decoder_surfaces)'
```

Hardware tests can return early on a host without a usable driver. Check their
output for `skipping:` before treating a passing run as hardware verification.
Nix-built tests also need their runtime loaders and drivers to be discoverable:
`LD_LIBRARY_PATH` for libva and Vulkan, `LIBVA_DRIVERS_PATH` for the Intel media
driver, and `VK_DRIVER_FILES` for the Intel Vulkan ICD. Use matching Nix
libraries and drivers to avoid mixing incompatible host dependencies.

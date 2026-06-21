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

Per-platform, picked at compile time:

- **macOS**: AVFoundation (camera) and ScreenCaptureKit (display), yielding
  zero-copy `CVPixelBuffer` surfaces straight to VideoToolbox.
- **Linux**: native V4L2 (camera; YUYV resampled, MJPEG via `zune-jpeg`).
- **Windows**: native Media Foundation (camera; `IMFSourceReader`) and DXGI
  Desktop Duplication (display; BGRA -> CPU I420). Display capture is
  whole-monitor; select one with a bare index or `display:{index}`.

## Encode

The codec is chosen via `encode::Codec`. Backends are tried in order (hardware
first, then software) and the first that opens wins; `encode::Kind` narrows the
choice (`Auto` / `Hardware` / `Software` / a named backend).

| Codec | Software | macOS | Windows | Linux |
|---|---|---|---|---|
| H.264 | openh264 (vendored, static) | VideoToolbox | Media Foundation | NVENC (`nvenc`), VAAPI (`vaapi`) |
| H.265 | none | VideoToolbox | Media Foundation | NVENC (`nvenc`) |

Every backend emits Annex-B with in-band parameter sets (SPS/PPS, plus VPS for
H.265), so the matching `moq_mux::codec` importer handles framing and catalog
registration directly. There is no software H.265 encoder (it's hardware-only).

Two public entry points:

- `encode::publish_capture(...)` captures a webcam, encodes it, and publishes on
  demand: the track and catalog are advertised up front, but the camera opens
  only while a subscriber is watching and is released when the last one leaves.
- `encode::Producer` publishes packets you encoded yourself, handling the catalog
  and framing.

The NVENC and VAAPI backends are Linux-only and gated behind their respective
features. Both `dlopen` the vendor driver at runtime (and fall back to software
where the driver is absent), so a feature-enabled binary still links on a
GPU-less builder and still starts on a GPU-less machine.

## Decode

`decode::Consumer` (the mirror of `moq-audio`'s `AudioConsumer`) subscribes to an
H.264 or H.265 track and returns raw I420 frames. Backends are tried
hardware-first, like encode:

| Codec | Software | macOS | Windows | Linux |
|---|---|---|---|---|
| H.264 | openh264 (vendored, static) | VideoToolbox | Media Foundation (DXVA) | openh264 |
| H.265 | none | none | Media Foundation (DXVA) | none |

On Windows the Microsoft decoder MFT runs synchronously with a Direct3D11 device
bound to it, so the decode happens on the GPU through DXVA (NVDEC / Intel / AMD).
H.264 falls back to openh264 on a GPU-less host; H.265 has no software decoder, so
it needs the GPU path (and an HEVC decoder MFT: the inbox HEVC Video Extensions or
a vendor one). A non-H.264/H.265 rendition yields `Error::UnsupportedCodec`.

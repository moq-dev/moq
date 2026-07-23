# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.8](https://github.com/moq-dev/moq/compare/moq-video-v0.0.7...moq-video-v0.0.8) - 2026-07-23

### Other

- *(rust)* pin the toolchain and correct the MSRV claims ([#2462](https://github.com/moq-dev/moq/pull/2462))

## [0.0.7](https://github.com/moq-dev/moq/compare/moq-video-v0.0.6...moq-video-v0.0.7) - 2026-07-22

### Added

- *(moq-video)* [**breaking**] adapt the encoder bitrate to the congestion-control estimate ([#2303](https://github.com/moq-dev/moq/pull/2303))
- *(capture)* macOS window/app/system-audio sources and device enumeration ([#2293](https://github.com/moq-dev/moq/pull/2293))
- *(moq-video)* PipeWire screen capture on Linux ([#2238](https://github.com/moq-dev/moq/pull/2238))
- *(moq-transcode)* decode once per source, GPU resize fanout, and a moq transcode verb ([#2158](https://github.com/moq-dev/moq/pull/2158))
- *(moq-video)* NVDEC hardware decode, zero-copy NVDEC -> NVENC transcode ([#2145](https://github.com/moq-dev/moq/pull/2145))
- moq-transcode, just-in-time transcoding for hang broadcasts (NVENC-capable) ([#2140](https://github.com/moq-dev/moq/pull/2140))
- *(moq-mux,moq-ffi)* catalog init from hints and configs ([#2102](https://github.com/moq-dev/moq/pull/2102))
- *(moq-net)* require broadcast close() + propagate real errors on abrupt teardown ([#2087](https://github.com/moq-dev/moq/pull/2087)) ([#2108](https://github.com/moq-dev/moq/pull/2108))
- *(moq-mux)* gate initial catalog publish until reserved tracks resolve ([#2072](https://github.com/moq-dev/moq/pull/2072))
- *(moq-video)* H.265 hardware decode on macOS (VideoToolbox) ([#1859](https://github.com/moq-dev/moq/pull/1859))
- *(moq-video)* opt-out nvenc/vaapi features (default-on) + correct libva docs ([#1860](https://github.com/moq-dev/moq/pull/1860))
- *(moq-video)* H.265 decode + Media Foundation HEVC backend ([#1854](https://github.com/moq-dev/moq/pull/1854))
- *(moq-video)* Windows screen capture via DXGI Desktop Duplication ([#1855](https://github.com/moq-dev/moq/pull/1855))
- *(moq-video)* H.264 hardware decode on Windows via Media Foundation ([#1853](https://github.com/moq-dev/moq/pull/1853))
- *(moq-video)* [**breaking**] make hardware encoders always-on (openh264 stays the software fallback) ([#1819](https://github.com/moq-dev/moq/pull/1819))
- *(moq-video)* NVENC H.265 encode + refresh README ([#1840](https://github.com/moq-dev/moq/pull/1840))
- *(moq-video,libmoq)* native H.264 decode (drop ffmpeg dependency) ([#1796](https://github.com/moq-dev/moq/pull/1796))
- *(moq-video)* H.265 (VideoToolbox) encode ([#1802](https://github.com/moq-dev/moq/pull/1802))
- *(moq-video)* replace cros-codecs git dep with the published moq-vaapi crate ([#1757](https://github.com/moq-dev/moq/pull/1757))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))
- *(moq-video)* mark the macOS Surface Sync so moq-transcode compiles ([#2225](https://github.com/moq-dev/moq/pull/2225))
- *(moq-video)* make decode Frame/Consumer Send on macOS ([#2162](https://github.com/moq-dev/moq/pull/2162))
- *(moq-video, moq-audio)* threading/correctness fixes + dedicated capture encode thread ([#2038](https://github.com/moq-dev/moq/pull/2038))
- *(moq-video, moq-audio)* non-contiguous VT output, blocking mic prompt, DXVA NV12 offset ([#2034](https://github.com/moq-dev/moq/pull/2034))
- *(moq-video)* make NVENC encode correct on hardware (forced IDR, in-band param sets, pitched input) ([#1997](https://github.com/moq-dev/moq/pull/1997))
- *(moq-video)* request camera access before capture on macOS ([#1803](https://github.com/moq-dev/moq/pull/1803))

### Other

- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(audio)* [**breaking**] align the moq-audio capture/encode surface with moq-video ([#2350](https://github.com/moq-dev/moq/pull/2350))
- align media docs and priorities ([#2336](https://github.com/moq-dev/moq/pull/2336))
- add NVDEC AV1 decode support ([#2178](https://github.com/moq-dev/moq/pull/2178))
- carry moq-video decode timestamps as moq_net::Timestamp ([#2146](https://github.com/moq-dev/moq/pull/2146))
- Factor stats snapshot types
- *(moq-net)* split flat type names into role modules ([#2070](https://github.com/moq-dev/moq/pull/2070))
- *(moq-video)* vendor NVENC fork in-tree as moq-nvenc ([#2042](https://github.com/moq-dev/moq/pull/2042))
- Merge main into dev
- Merge main into dev
- *(moq-net)* make request_broadcast/subscribe/fetch_group infallible ([#1890](https://github.com/moq-dev/moq/pull/1890))
- *(moq-video, moq-audio)* make device capture async (fixes Ctrl+C shutdown hang) ([#1807](https://github.com/moq-dev/moq/pull/1807))
- decouple importers from the catalog, split byte-parsing into per-codec splitters, and make importers pure frame publishers ([#1749](https://github.com/moq-dev/moq/pull/1749))
- Merge origin/main into dev
- *(moq-video)* cover the Windows hardware encoder ([#1740](https://github.com/moq-dev/moq/pull/1740))
- Merge remote-tracking branch 'origin/dev' into claude/epic-hamilton-e9edf7
- Merge branch 'main' into dev

### Added

- `decode::Frame::resize(width, height)`: a scaled copy of a decoded frame,
  preserving the timestamp. A CUDA frame (NVDEC output) resizes on the GPU with
  a box-filter kernel (vendored PTX, JIT-compiled by the driver; no CUDA
  toolkit needed to build) and stays in device memory; CPU frames resize with a
  SIMD bilinear convolution. Fans one decoded stream out to several sizes, e.g.
  a transcode ladder sharing one decoder.

- H.264 / H.265 hardware decode on Linux via NVIDIA NVDEC, behind the
  default-on `nvdec` feature. Decoded frames stay in CUDA device memory and
  feed the NVENC encoder zero-copy through the new
  `encode::Encoder::encode(frame)` entry point; `decode::Config::resize` scales
  in the decoder for free. Like NVENC, everything is dlopen'd at runtime, so a
  driverless host falls back to the next decoder.
- AV1 hardware decode on Linux via NVDEC for 8-bit 4:2:0 sources. AV1 is
  decode-only and emits the same CUDA NV12 frame type as H.264/H.265, so it can
  feed existing NVENC H.264/H.265 transcode output.
- `decode::Frame` pixels are now private: `into_i420()` returns the packed
  I420 bytes (downloading a GPU frame), replacing the public `data` field, and
  each frame's `timestamp_us` now rides the decoder (correct across reordering)
  instead of echoing the input.
- `decode::Decoder::new` takes the full `decode::Config` instead of just the
  `Kind`, so decoder knobs (like `resize`) stay additive.

- `decode::Decoder` is public: the payload-in, frames-out layer under
  `decode::Consumer`, for callers that don't read from a plain track
  subscription (e.g. a transcoder decoding individually fetched groups).
- `encode::Encoder::encode_i420`: encode a tightly-packed I420 frame directly,
  the zero-conversion input path for callers that already hold I420 (decoder
  output), alongside the existing `encode_rgba`.
- Native H.264 decode: a `decode` module mirroring `encode`, with a
  `decode::Consumer` (the counterpart to `moq-audio`'s `AudioConsumer`) that
  subscribes to an H.264 track and returns raw I420 frames. Backends are
  VideoToolbox (macOS) and openh264 (portable software fallback); no ffmpeg.
- H.264 hardware decode on Windows via Media Foundation. The Microsoft decoder
  MFT runs synchronously with a Direct3D11 device bound to it, so the decode
  happens on the GPU through DXVA (NVDEC / Intel / AMD); output textures are
  downloaded to I420. Requires a GPU: a GPU-less host falls back to openh264.
- Windows screen capture (`capture::Source::Display`) via DXGI Desktop
  Duplication. Duplicates a monitor on a Direct3D11 device, copies each desktop
  frame to a staging texture, and converts BGRA to I420. Whole-monitor capture;
  select one with a bare index or `display:{index}`. The read loop paces to the
  target frame rate and re-emits the last frame while the screen is static.
- H.265 decode: the `decode` module now handles H.265 tracks (hvc1 and hev1)
  alongside H.264, sharing the same length-prefixed -> Annex-B front end.
  VideoToolbox (macOS) and Media Foundation (Windows, DXVA) decode it on
  hardware, pulling VPS/SPS/PPS out of each keyframe to build the format
  description. There is no software H.265 decoder, so H.265 has no fallback below
  the hardware path. The macOS VideoToolbox path is verified by an end-to-end
  HEVC encode -> decode round-trip on Apple silicon; the Windows path is
  unverified on hardware (the test box had no HEVC decoder MFT installed).
- H.265 encode via the NVENC backend (Linux, `nvenc` feature). The codec is
  selected by `encode::Codec`; the NVENC HEVC path shares the H.264 preset / GOP
  / rate-control setup and emits Annex-B with inline VPS/SPS/PPS.
- NVENC H.264/H.265 encode verified end-to-end on a Linux + NVIDIA box (RTX 30
  series), which fixed three correctness bugs the software-only path had hidden:
  a forced keyframe now emits an IDR (via the `FORCEIDR` picture flag, since
  picture-type decision makes NVENC ignore `pictureType`); every IDR, not just
  the first, carries inline SPS/PPS (VPS too for HEVC) so a mid-stream subscriber
  can join at any keyframe (`repeatSPSPPS` + `idrPeriod`); and the input frame is
  copied at NVENC's real buffer pitch (e.g. 512 for a 320-wide buffer) instead of
  a flat copy that sheared the image, which also drops the former width-multiple-
  of-64 restriction. Requires a matching `moq-nvenc` change
  (`force_idr` flag + pitched `BufferLock::write_rows`).

## [0.0.6](https://github.com/moq-dev/moq/compare/moq-video-v0.0.5...moq-video-v0.0.6) - 2026-06-30

### Other

- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

## [0.0.5](https://github.com/moq-dev/moq/compare/moq-video-v0.0.4...moq-video-v0.0.5) - 2026-06-23

### Added

- *(catalog)* expose untyped catalog extensions via moq-ffi and libmoq ([#1886](https://github.com/moq-dev/moq/pull/1886))

## [0.0.4](https://github.com/moq-dev/moq/compare/moq-video-v0.0.3...moq-video-v0.0.4) - 2026-06-16

### Other

- *(moq-cli)* remove the capture feature ([#1728](https://github.com/moq-dev/moq/pull/1728))

## [0.0.3](https://github.com/moq-dev/moq/compare/moq-video-v0.0.2...moq-video-v0.0.3) - 2026-06-10

### Added

- *(moq-video,moq-cli)* webcam capture and publish ([#1669](https://github.com/moq-dev/moq/pull/1669))

### Added

- Webcam capture via libavdevice, hardware-preferred H.264 encoding via ffmpeg
  (`encode::Encoder`), and an `encode::Producer` / `encode::publish_capture`
  pipeline that publishes through `moq_mux::codec::h264::Import`. Wired into
  `moq-cli` as the `capture` publish subcommand (behind the `capture` feature).
- `encode::publish_capture` encodes on demand: the track/catalog are advertised
  up front but the camera opens only while a subscriber is watching (mirroring
  `moq-boy`'s `TrackProducer::used()` / `unused()` gating) and is released when idle.

## [0.0.2](https://github.com/moq-dev/moq/compare/moq-codec-v0.0.1...moq-codec-v0.0.2) - 2026-04-03

### Other

- Add moq-relay release workflow and Nix cache configuration ([#1178](https://github.com/moq-dev/moq/pull/1178))

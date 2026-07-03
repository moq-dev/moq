# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
  of-64 restriction. Requires a matching `nvidia-video-codec-sdk` fork bump
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

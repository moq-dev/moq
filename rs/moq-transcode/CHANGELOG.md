# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.3](https://github.com/moq-dev/moq/compare/moq-transcode-v0.0.2...moq-transcode-v0.0.3) - 2026-07-24

### Other

- *(moq-video)* bump moq-vaapi to 0.0.3 (dlopen libva) ([#2465](https://github.com/moq-dev/moq/pull/2465))

## [0.0.2](https://github.com/moq-dev/moq/compare/moq-transcode-v0.0.1...moq-transcode-v0.0.2) - 2026-07-23

### Other

- updated the following local packages: moq-video

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-transcode-v0.0.1) - 2026-07-22

### Added

- *(moq-net)* let finish_at declare a future exclusive end group ([#2219](https://github.com/moq-dev/moq/pull/2219)) ([#2234](https://github.com/moq-dev/moq/pull/2234))
- *(moq-transcode)* decode once per source, GPU resize fanout, and a moq transcode verb ([#2158](https://github.com/moq-dev/moq/pull/2158))
- *(moq-video)* NVDEC hardware decode, zero-copy NVDEC -> NVENC transcode ([#2145](https://github.com/moq-dev/moq/pull/2145))
- moq-transcode, just-in-time transcoding for hang broadcasts (NVENC-capable) ([#2140](https://github.com/moq-dev/moq/pull/2140))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))
- *(moq-video)* mark the macOS Surface Sync so moq-transcode compiles ([#2225](https://github.com/moq-dev/moq/pull/2225))

### Other

- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- *(audio)* [**breaking**] align the moq-audio capture/encode surface with moq-video ([#2350](https://github.com/moq-dev/moq/pull/2350))
- *(hang)* [**breaking**] non_exhaustive catalog sections, shared container::track_info, hang draft catch-up ([#2341](https://github.com/moq-dev/moq/pull/2341))
- moq-net + js/net: pre-merge API hardening for moq-lite-05 ([#2170](https://github.com/moq-dev/moq/pull/2170))
- add NVDEC AV1 decode support ([#2178](https://github.com/moq-dev/moq/pull/2178))
- carry moq-video decode timestamps as moq_net::Timestamp ([#2146](https://github.com/moq-dev/moq/pull/2146))

### Added

- Shared live decode: all rungs of a source with live demand now share one
  subscription and one decoder (a broadcast feed of decoded frames), instead of
  each rung decoding the source independently. NVDEC throughput and upstream
  bandwidth now scale with source count, not ladder depth; each rung resizes
  its copy on the GPU (`decode::Frame::resize`) and encodes it in place. Group
  fetches keep their own one-shot pipeline.
- `moq transcode`: the transcoder is now a moq-cli verb (behind the `transcode`
  feature), publishing `<broadcast>/transcode.hang` with a configurable ladder
  (`--rung height:bitrate`) and codec pins (`--encoder`, `--decoder`).

- Initial release: just-in-time live transcoding of hang broadcasts.
  `run(source, output, config)` publishes a derivative catalog (ladder rungs
  strictly below the source, plus relative references to the source renditions)
  and encodes each rung only while it is subscribed or fetched. Output groups
  mirror source group sequence numbers, so specific-group fetches map 1:1 to
  source groups. Encoding via `moq-video` (NVENC/VideoToolbox/Media Foundation
  hardware, openh264 fallback). On an NVIDIA GPU the pipeline is zero-copy:
  NVDEC decodes and scales in hardware and NVENC encodes the CUDA frame in
  place; other decoders scale I420 on the CPU.
- 8-bit 4:2:0 AV1 source renditions are eligible for transcoding when a native
  decoder is available. On Linux with NVDEC, AV1 sources can feed existing
  H.264/H.265 output rungs.

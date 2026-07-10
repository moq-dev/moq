# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

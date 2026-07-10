# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial release: just-in-time live transcoding of hang broadcasts.
  `run(source, output, config)` publishes a derivative catalog (ladder rungs
  strictly below the source, plus relative references to the source renditions)
  and encodes each rung only while it is subscribed or fetched. Output groups
  mirror source group sequence numbers, so specific-group fetches map 1:1 to
  source groups. Encoding via `moq-video` (NVENC/VideoToolbox/Media Foundation
  hardware, openh264 fallback). On an NVIDIA GPU the pipeline is zero-copy:
  NVDEC decodes and scales in hardware and NVENC encodes the CUDA frame in
  place; other decoders scale I420 on the CPU.

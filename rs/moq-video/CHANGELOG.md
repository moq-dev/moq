# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

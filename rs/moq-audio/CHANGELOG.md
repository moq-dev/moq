# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.4](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.3...moq-audio-v0.0.4) - 2026-06-16

### Fixed

- *(moq-audio)* surface denied/unavailable mic instead of hanging ([#1708](https://github.com/moq-dev/moq/pull/1708))

## [0.0.3](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.2...moq-audio-v0.0.3) - 2026-06-10

### Added

- *(moq-video,moq-cli)* webcam capture and publish ([#1669](https://github.com/moq-dev/moq/pull/1669))
- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))

### Added

- `capture` feature: `capture::Microphone` captures an input device via cpal
  (pure-Rust: CoreAudio / WASAPI / ALSA) yielding PCM frames, and
  `capture::publish_microphone` runs the mic -> Opus -> publish loop on demand
  (the catalog is registered up front from the device format, but the mic only
  opens while a subscriber is listening). Off by default so audio-only consumers
  don't pull cpal / ALSA. Encoding stays on unsafe-libopus.
- `AudioProducer` timestamps are now anchored to the first frame's wall clock,
  with `reset_epoch()` to re-anchor after an idle gap (so a released-and-reopened
  microphone stays aligned with a wall-clock video track rather than compressing
  the gap out). Mirrors moq-boy.

## [0.0.2](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.1...moq-audio-v0.0.2) - 2026-06-03

### Other

- *(deps)* bump the cargo group (with code fixes for rand/rubato/rcgen) ([#1603](https://github.com/moq-dev/moq/pull/1603))

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-audio-v0.0.1) - 2026-05-24

### Added

- add moq-audio crate, raw-audio FFI, and rename moq-codec to moq-video ([#1484](https://github.com/moq-dev/moq/pull/1484))

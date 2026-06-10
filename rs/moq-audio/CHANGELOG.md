# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `capture` feature: `capture::Microphone` captures an input device via cpal
  (pure-Rust: CoreAudio / WASAPI / ALSA) yielding PCM frames, and
  `capture::publish_microphone` runs the mic -> Opus -> publish loop. Off by
  default so audio-only consumers don't pull cpal / ALSA. Encoding stays on
  unsafe-libopus.

## [0.0.2](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.1...moq-audio-v0.0.2) - 2026-06-03

### Other

- *(deps)* bump the cargo group (with code fixes for rand/rubato/rcgen) ([#1603](https://github.com/moq-dev/moq/pull/1603))

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-audio-v0.0.1) - 2026-05-24

### Added

- add moq-audio crate, raw-audio FFI, and rename moq-codec to moq-video ([#1484](https://github.com/moq-dev/moq/pull/1484))

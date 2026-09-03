# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- [**breaking**] *(moq-audio)* report Opus discontinuous transmission as an `Activity` on the audio itself: `Frame` gains an `activity` field (build one with the new `Frame::new`), `Encoder::encode` returns `encode::Encoded`, `Decoder::decode` returns `decode::Decoded`, and `encode::Producer::activity` reports what was published most recently ([#2481](https://github.com/moq-dev/moq/issues/2481))
- [**breaking**] *(moq-audio)* reject an Opus bitrate too low for the frame duration to code any audio, which libopus otherwise accepts and answers with empty frames

## [0.0.21](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.20...moq-audio-v0.0.21) - 2026-09-01

### Fixed

- *(moq-audio)* keep capture callbacks realtime-safe ([#3245](https://github.com/moq-dev/moq/pull/3245))
- *(moq-audio)* bound playback driver commands ([#3170](https://github.com/moq-dev/moq/pull/3170))
- *(audio)* recover microphone capture after device errors ([#3179](https://github.com/moq-dev/moq/pull/3179))

### Other

- *(rs)* point shared dependencies at [workspace.dependencies] ([#3098](https://github.com/moq-dev/moq/pull/3098))

## [0.0.20](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.19...moq-audio-v0.0.20) - 2026-08-26

### Other

- updated the following local packages: moq-net, moq-mux, hang

## [0.0.19](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.18...moq-audio-v0.0.19) - 2026-08-24

### Added

- *(audio)* decode AAC-LC ([#2968](https://github.com/moq-dev/moq/pull/2968))

### Fixed

- *(audio)* drain Opus lookahead on finish ([#3008](https://github.com/moq-dev/moq/pull/3008))
- *(audio)* make the resampler tell the truth about where its samples belong ([#2992](https://github.com/moq-dev/moq/pull/2992))

## [0.0.18](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.17...moq-audio-v0.0.18) - 2026-08-20

### Other

- updated the following local packages: moq-mux

## [0.0.17](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.16...moq-audio-v0.0.17) - 2026-08-07

### Added

- *(cli)* add moq play ([#2697](https://github.com/moq-dev/moq/pull/2697))

## [0.0.16](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.15...moq-audio-v0.0.16) - 2026-08-06

### Added

- *(hang)* declare a configurable 30s retention on media tracks, and fix relayed cache misses ([#2615](https://github.com/moq-dev/moq/pull/2615))
- fail-fast retries: jittered backoff bounded by time, not error type ([#2647](https://github.com/moq-dev/moq/pull/2647))

## [0.0.15](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.14...moq-audio-v0.0.15) - 2026-07-29

### Added

- *(moq-audio)* cancel the speaker's echo out of the microphone ([#2538](https://github.com/moq-dev/moq/pull/2538))

## [0.0.14](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.13...moq-audio-v0.0.14) - 2026-07-27

### Added

- *(moq-audio)* play decoded PCM out a speaker ([#2529](https://github.com/moq-dev/moq/pull/2529))

## [0.0.13](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.12...moq-audio-v0.0.13) - 2026-07-25

### Added

- *(moq-mux)* caller-driven audio grouping via dumb importers ([#2496](https://github.com/moq-dev/moq/pull/2496))
- *(moq-audio)* add PCM codec ([#2493](https://github.com/moq-dev/moq/pull/2493))

### Fixed

- *(moq-audio)* bound capture buffer queue ([#2487](https://github.com/moq-dev/moq/pull/2487))
- *(opus)* propagate pre-skip and encoder controls ([#2492](https://github.com/moq-dev/moq/pull/2492))

## [0.0.12](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.11...moq-audio-v0.0.12) - 2026-07-24

### Added

- *(moq-mux,moq-boy)* mark discontinuities, and never time a sample across one ([#2475](https://github.com/moq-dev/moq/pull/2475))

## [0.0.11](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.10...moq-audio-v0.0.11) - 2026-07-23

### Other

- *(rust)* pin the toolchain and correct the MSRV claims ([#2462](https://github.com/moq-dev/moq/pull/2462))

## [0.0.10](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.9...moq-audio-v0.0.10) - 2026-07-22

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))

### Other

- *(mux)* [**breaking**] unseal catalog renditions and make timelines explicit/shareable ([#2420](https://github.com/moq-dev/moq/pull/2420))
- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- Merge remote-tracking branch 'origin/main' into dev
- *(deps)* bump the cargo group with 2 updates ([#2409](https://github.com/moq-dev/moq/pull/2409))

## [0.0.9](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.8...moq-audio-v0.0.9) - 2026-07-16

### Added

- *(moq-mux)* cut(end) as the group boundary ([#2270](https://github.com/moq-dev/moq/pull/2270))

## [0.0.8](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.7...moq-audio-v0.0.8) - 2026-07-09

### Other

- Per-track timeline index for each media track ([#2109](https://github.com/moq-dev/moq/pull/2109))

## [0.0.7](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.6...moq-audio-v0.0.7) - 2026-07-04

### Other

- [codex] Future-proof moq-net metadata structs ([#2046](https://github.com/moq-dev/moq/pull/2046))

## [0.0.6](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.5...moq-audio-v0.0.6) - 2026-06-30

### Other

- API cleanup before the semver bump ([#1941](https://github.com/moq-dev/moq/pull/1941))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

## [0.0.5](https://github.com/moq-dev/moq/compare/moq-audio-v0.0.4...moq-audio-v0.0.5) - 2026-06-23

### Other

- updated the following local packages: moq-mux

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

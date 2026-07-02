# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Native video decode C API: `moq_consume_video_raw` (+ `_close`, `_frame`,
  `_frame_free`) subscribes to an H.264 track and hands back decoded I420 frames,
  the video counterpart to `moq_consume_audio_raw`. Decoding happens inside
  libmoq (VideoToolbox / openh264), so consumers no longer need ffmpeg.

## [0.3.9](https://github.com/moq-dev/moq/compare/libmoq-v0.3.8...libmoq-v0.3.9) - 2026-06-30

### Other

- API cleanup before the semver bump ([#1941](https://github.com/moq-dev/moq/pull/1941))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

## [0.3.8](https://github.com/moq-dev/moq/compare/libmoq-v0.3.7...libmoq-v0.3.8) - 2026-06-23

### Added

- *(catalog)* expose untyped catalog extensions via moq-ffi and libmoq ([#1886](https://github.com/moq-dev/moq/pull/1886))

### Fixed

- link macOS CoreServices for the bundled notify/FSEvents backend ([#1875](https://github.com/moq-dev/moq/pull/1875))

## [0.3.7](https://github.com/moq-dev/moq/compare/libmoq-v0.3.6...libmoq-v0.3.7) - 2026-06-19

### Fixed

- *(libmoq)* use .cast() for c_char pointer to fix arm64 clippy ([#1782](https://github.com/moq-dev/moq/pull/1782))

## [0.3.5](https://github.com/moq-dev/moq/compare/libmoq-v0.3.4...libmoq-v0.3.5) - 2026-06-16

### Fixed

- *(native)* surface terminal auth connect errors ([#1649](https://github.com/moq-dev/moq/pull/1649))

## [0.3.4](https://github.com/moq-dev/moq/compare/libmoq-v0.3.3...libmoq-v0.3.4) - 2026-06-10

### Added

- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))

### Fixed

- *(moq-relay)* classify malformed auth-API JSON as an upstream 502

### Other

- Revert accidental commit 24d25604 (moq-native connect/reconnect refactor)
- *(moq-native)* migrate from anyhow to thiserror ([#1651](https://github.com/moq-dev/moq/pull/1651))
- cross-compile all x86_64-darwin release artifacts on Apple Silicon ([#1623](https://github.com/moq-dev/moq/pull/1623))

## [0.3.3](https://github.com/moq-dev/moq/compare/libmoq-v0.3.2...libmoq-v0.3.3) - 2026-06-03

### Other

- updated the following local packages: moq-audio

## [0.3.2](https://github.com/moq-dev/moq/compare/libmoq-v0.3.1...libmoq-v0.3.2) - 2026-06-02

### Other

- expose moq_error(), stop logging FFI errors ([#1586](https://github.com/moq-dev/moq/pull/1586))
- shrink moq-ffi & libmoq staticlibs with LTO (unblocks the moq-go mirror push) ([#1577](https://github.com/moq-dev/moq/pull/1577))

## [0.3.1](https://github.com/moq-dev/moq/compare/libmoq-v0.3.0...libmoq-v0.3.1) - 2026-05-30

### Other

- add moq_origin_consume_announced to wait for a broadcast ([#1552](https://github.com/moq-dev/moq/pull/1552))
- route Android logs to logcat ([#1541](https://github.com/moq-dev/moq/pull/1541))

## [0.3.0](https://github.com/moq-dev/moq/compare/libmoq-v0.2.17...libmoq-v0.3.0) - 2026-05-30

### Other

- terminal-callback lifetime contract for C consumers ([#1546](https://github.com/moq-dev/moq/pull/1546))
- auto-reconnect sessions; conducer-based Reconnect notifications ([#1544](https://github.com/moq-dev/moq/pull/1544))
- Add libmoq catalog producer + raw moq-net track API ([#1533](https://github.com/moq-dev/moq/pull/1533))
- lint shell, workflows, TOML, Nix, and justfiles via nix devShell ([#1519](https://github.com/moq-dev/moq/pull/1519))

### Added

- Catalog producer API to author renditions directly (`moq_publish_video_config`, `moq_publish_audio_config`, `moq_publish_video_remove`, `moq_publish_audio_remove`), mirroring the consume-side config queries.
- Raw moq-net track API for arbitrary (non-media) byte tracks, mirroring the moq-ffi primitives:
  - Publish: `moq_publish_track`, `moq_publish_track_group`, `moq_publish_track_frame`, `moq_publish_group_frame`, `moq_publish_group_close`, `moq_publish_track_close`.
  - Consume: `moq_consume_track`, `moq_consume_track_frame`, `moq_consume_track_frame_close`, `moq_consume_track_close`.

## [0.2.17](https://github.com/moq-dev/moq/compare/libmoq-v0.2.16...libmoq-v0.2.17) - 2026-05-24

### Added

- add moq-audio crate, raw-audio FFI, and rename moq-codec to moq-video ([#1484](https://github.com/moq-dev/moq/pull/1484))

## [0.2.16](https://github.com/moq-dev/moq/compare/libmoq-v0.2.15...libmoq-v0.2.16) - 2026-05-23

### Other

- Package moq-gst for release via Nix-built tarballs ([#1453](https://github.com/moq-dev/moq/pull/1453))

## [0.2.15](https://github.com/moq-dev/moq/compare/libmoq-v0.2.14...libmoq-v0.2.15) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.2.14](https://github.com/moq-dev/moq/compare/libmoq-v0.2.13...libmoq-v0.2.14) - 2026-05-07

### Other

- moq-mux backport + dual-API cleanup ([#1341](https://github.com/moq-dev/moq/pull/1341))
- tighten public API surface and remove deprecated methods ([#1378](https://github.com/moq-dev/moq/pull/1378))
- Revert moq-lite FETCH/Subscription API changes ([#1372](https://github.com/moq-dev/moq/pull/1372))
- backport Subscription model API for FETCH readiness ([#1348](https://github.com/moq-dev/moq/pull/1348))
- add OriginConsumer::wait_for_broadcast; deprecate consume_broadcast ([#1340](https://github.com/moq-dev/moq/pull/1340))
- hop-based clustering ([#1322](https://github.com/moq-dev/moq/pull/1322))

## [0.2.13](https://github.com/moq-dev/moq/compare/libmoq-v0.2.12...libmoq-v0.2.13) - 2026-03-18

### Other

- Fix FFI test panic strategy mismatch ([#1128](https://github.com/moq-dev/moq/pull/1128))
- Remove unused dev-dependencies and bump @moq/qmux ([#1126](https://github.com/moq-dev/moq/pull/1126))

## [0.2.12](https://github.com/moq-dev/moq/compare/libmoq-v0.2.11...libmoq-v0.2.12) - 2026-03-13

### Other

- Validate libmoq IDs fit in i32 at creation time ([#1087](https://github.com/moq-dev/moq/pull/1087))
- Fix libmoq test races by using monotonic IDs ([#1086](https://github.com/moq-dev/moq/pull/1086))
- Set MSRV to 1.85 (edition 2024) ([#1083](https://github.com/moq-dev/moq/pull/1083))
- Add comprehensive FFI integration tests for libmoq broadcast ([#1068](https://github.com/moq-dev/moq/pull/1068))
- Improve libmoq C bindings ([#1061](https://github.com/moq-dev/moq/pull/1061))

## [0.2.10](https://github.com/moq-dev/moq/compare/libmoq-v0.2.9...libmoq-v0.2.10) - 2026-03-03

### Other

- OrderedProducer API with max_group_duration ([#1007](https://github.com/moq-dev/moq/pull/1007))
- Add typed initialization for Opus and AAC in moq-mux ([#1034](https://github.com/moq-dev/moq/pull/1034))
- Add moq-msf crate for MSF catalog support ([#993](https://github.com/moq-dev/moq/pull/993))
- Replace tokio::sync::watch with custom Producer/Subscriber ([#996](https://github.com/moq-dev/moq/pull/996))

## [0.2.8](https://github.com/moq-dev/moq/compare/libmoq-v0.2.7...libmoq-v0.2.8) - 2026-02-12

### Other

- Error cleanup ([#944](https://github.com/moq-dev/moq/pull/944))
- Reduce the moq-lite API size ([#943](https://github.com/moq-dev/moq/pull/943))

## [0.2.7](https://github.com/moq-dev/moq/compare/libmoq-v0.2.6...libmoq-v0.2.7) - 2026-02-09

### Other

- Use `moq` instead of `hang` for some crates ([#906](https://github.com/moq-dev/moq/pull/906))
- Remove priority from the catalog ([#905](https://github.com/moq-dev/moq/pull/905))

## [0.2.6](https://github.com/moq-dev/moq/compare/libmoq-v0.2.5...libmoq-v0.2.6) - 2026-02-03

### Other

- updated the following local packages: moq-lite, hang

## [0.2.5](https://github.com/moq-dev/moq/compare/libmoq-v0.2.4...libmoq-v0.2.5) - 2026-01-24

### Other

- Add a builder pattern for constructing clients/servers ([#862](https://github.com/moq-dev/moq/pull/862))
- Add universal libmoq build for macos  ([#861](https://github.com/moq-dev/moq/pull/861))
- Add #[non_exhaustive] to moq-native configuration. ([#850](https://github.com/moq-dev/moq/pull/850))
- upgrade to Rust edition 2024 ([#838](https://github.com/moq-dev/moq/pull/838))

## [0.2.4](https://github.com/moq-dev/moq/compare/libmoq-v0.2.3...libmoq-v0.2.4) - 2026-01-12

## [0.2.3](https://github.com/moq-dev/moq/compare/libmoq-v0.2.2...libmoq-v0.2.3) - 2026-01-10

### Added

- iroh support ([#794](https://github.com/moq-dev/moq/pull/794))

### Other

- Add generic time system with Timescale type ([#824](https://github.com/moq-dev/moq/pull/824))
- support WebSocket fallback for clients ([#812](https://github.com/moq-dev/moq/pull/812))
- target_link_libraries ([#802](https://github.com/moq-dev/moq/pull/802))

## [0.2.2](https://github.com/moq-dev/moq/compare/libmoq-v0.2.1...libmoq-v0.2.2) - 2025-12-19

### Other

- Add HLS import module ([#789](https://github.com/moq-dev/moq/pull/789))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/libmoq-v0.1.0) - 2025-12-13

### Other

- Use BufList for hang::Frame ([#769](https://github.com/moq-dev/moq/pull/769))
- Fix and over-optimize the H.264 annex.b import ([#766](https://github.com/moq-dev/moq/pull/766))
- Don't use 0 index for the slab. ([#758](https://github.com/moq-dev/moq/pull/758))
- Fix the include.h path ([#755](https://github.com/moq-dev/moq/pull/755))
- kixelated -> moq-dev ([#749](https://github.com/moq-dev/moq/pull/749))
- Revamp the C API and have it use hang/import ([#732](https://github.com/moq-dev/moq/pull/732))

## [0.7.0](https://github.com/moq-dev/moq/compare/libmoq-v0.6.1...libmoq-v0.7.0) - 2025-11-26

### Other

- Add initial C bindings for moq ([#722](https://github.com/kixelated/moq/pull/722))

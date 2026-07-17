# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Linking `libmoq.a` on macOS no longer fails on undefined Apple framework
  symbols, and the CMake package config carries the same libraries the
  pkg-config file does.
- `moq_track_info.timescale_valid` documented that it overrides a default
  millisecond timescale. The default is and was microseconds, matching the
  `timestamp_us` units used everywhere else in the ABI. Only the doc was wrong.

### Changed

- Producer teardown is now spelled `_finish`, not `_close`, so the name states
  the end-of-stream semantics and no longer reads as a synonym for the `_abort`
  that sits next to it: `moq_publish_close` -> `moq_publish_finish`, and the
  same for `moq_publish_media_close`, `moq_publish_track_close`,
  `moq_publish_group_close`, `moq_publish_json_snapshot_close`,
  `moq_publish_json_stream_close`, and `moq_publish_audio_raw_close`.
  `moq_publish_track_finish` now also pairs with `moq_publish_track_finish_at`.
  `_close` keeps its other two meanings (stop a listener, close a connection),
  so `moq_consume_*_close`, `moq_origin_*_close`, and `moq_session_close` are
  unchanged.
- `moq_origin_publish` / `moq_origin_unpublish` -> `moq_origin_announce` /
  `moq_origin_unannounce`, so the C ABI uses the same announce verb as every
  other layer. The `origin_publish` / `origin_consume` parameters of
  `moq_session_connect` keep their names: they name a direction, not this
  operation.
- `moq_remove_catalog_section` -> `moq_publish_catalog_section_remove`, putting
  it under the `moq_publish_catalog_section` sibling it belongs to instead of
  breaking the verb-prefix scheme.
- Dropped the `_ordered` suffix, which leaked a long-gone internal type:
  `moq_publish_media_ordered` -> `moq_publish_media`,
  `moq_consume_video_ordered` -> `moq_consume_video`, and
  `moq_consume_audio_ordered` -> `moq_consume_audio`. These now match the
  `publish_media` / `subscribe_media` names moq-ffi already uses.
- The native libraries an external linker needs alongside `libmoq.a` now come
  from `rs/libmoq/native-libs/`, so the pkg-config file and the CMake package
  config can no longer drift apart. This adds the Apple media frameworks, the
  capture frameworks, and the C++ runtime on macOS, `libva` and the C++ runtime
  on Linux, and the full system library set on Windows.
- JSON snapshot C ABI renamed for symmetry with the stream mode, so the caller
  opts explicitly into one of the two modes: `moq_json_config` ->
  `moq_json_snapshot_config`, `moq_publish_json` -> `moq_publish_json_snapshot`
  (and `_update` / `_finish`), and `moq_consume_json` ->
  `moq_consume_json_snapshot`. The shared `moq_json_value`,
  `moq_consume_json_value{,_close}`, and `moq_consume_json_close` are unchanged.

### Added

- Raw track APIs for explicit group sequences, known track ends, and track or
  group aborts.
- Raw track options for the C ABI: `moq_publish_track` now accepts
  `moq_track_info`, and `moq_consume_track` now accepts `moq_subscription`;
  subscriptions can be updated with `moq_consume_track_update`.
- Native video decode C API: `moq_consume_video_raw` (+ `_close`, `_frame`,
  `_frame_free`) subscribes to an H.264 track and hands back decoded I420 frames,
  the video counterpart to `moq_consume_audio_raw`. Decoding happens inside
  libmoq (VideoToolbox / openh264), so consumers no longer need ffmpeg.

## [0.3.12](https://github.com/moq-dev/moq/compare/libmoq-v0.3.11...libmoq-v0.3.12) - 2026-07-12

### Other

- split into snapshot/stream modules and expose JSON tracks through moq-ffi/libmoq ([#2196](https://github.com/moq-dev/moq/pull/2196))

## [0.3.11](https://github.com/moq-dev/moq/compare/libmoq-v0.3.10...libmoq-v0.3.11) - 2026-07-09

### Other

- updated the following local packages: moq-audio

## [0.3.10](https://github.com/moq-dev/moq/compare/libmoq-v0.3.9...libmoq-v0.3.10) - 2026-07-04

### Other

- [codex] Future-proof moq-net metadata structs ([#2046](https://github.com/moq-dev/moq/pull/2046))
- allowing container be probed and select depending on the what on wire ([#2040](https://github.com/moq-dev/moq/pull/2040))

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

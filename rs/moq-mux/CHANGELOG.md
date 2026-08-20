# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- `import::ContainerStream::new` takes a bare `ContainerFormat` instead of a `ContainerInit`. It
  only ever read the format, so the init's leading bytes were accepted and dropped. A stream
  recovers its own framing, so push everything through `decode` instead.

- Import formats are typed: `import::{AudioFormat, VideoFormat, ContainerFormat}` replace the format
  string on each `Init`, so a format of the wrong kind no longer compiles. `FromStr` accepts every
  alias and is where a caller crossing a string boundary finds out, reporting `Error::WrongKind`
  with the kind that does take it. `Display` gives the canonical name, so aliases like `h264` and
  `avc3` no longer produce differently named tracks.
- `Error::NotSelfDescribing` replaces the unknown-format error `TrackStream::video` raised for a
  video codec a raw byte stream cannot be split into.
- `import::Track::new` splits into `Track::audio` and `Track::video`, `import::TrackStream::new`
  becomes `TrackStream::video`, and `import::Init` splits into `AudioInit`, `VideoInit`, and
  `ContainerInit`. Each entry point takes only the fields its kind can honor, so a video hint on an
  audio import or a label on a container is no longer expressible.
- `Error::UnsupportedField` becomes `Error::WrongKind`, which names the kind that does handle the
  format rather than reporting it as unknown. `import::Kind::of` exposes the same classification.

### Added

- Propagate rendition labels through single-track media imports.
- `From<hang::catalog::VideoConfig> for catalog::VideoHint`, a total conversion for a caller that
  already has a whole rendition. Replaces the per-field copy in `moq-video`, which dropped the label.

### Changed

- `import::Container::new` and `import::ContainerStream::new` take an `import::Init` instead of a
  format and buffer, and reject the single-rendition `label` and `video` fields rather than
  dropping them.
- An audio format rejects an `import::Init` video hint instead of dropping it.
- `catalog::VideoHint::label` is no longer public. `import::Init::label` is the single source of a
  rendition label, matching the hang draft, which classifies it as a common rendition field.

## [0.9.7](https://github.com/moq-dev/moq/compare/moq-mux-v0.9.6...moq-mux-v0.9.7) - 2026-08-14

### Added

- *(bindings)* fetch and decode a retained media group ([#2827](https://github.com/moq-dev/moq/pull/2827))

### Fixed

- *(mux)* derive missing video geometry ([#2840](https://github.com/moq-dev/moq/pull/2840))
- *(path)* resolve catalog references like URLs ([#2855](https://github.com/moq-dev/moq/pull/2855))
- *(moq-mux)* stop publishing audio spliced across a TS discontinuity ([#2823](https://github.com/moq-dev/moq/pull/2823))
- *(moq-mux)* anchor the TS table cadence to the media timeline ([#2825](https://github.com/moq-dev/moq/pull/2825))

## [0.9.6](https://github.com/moq-dev/moq/compare/moq-mux-v0.9.5...moq-mux-v0.9.6) - 2026-08-13

### Added

- *(moq-video)* advertise the catalog rendition before the first keyframe ([#2768](https://github.com/moq-dev/moq/pull/2768))

### Fixed

- *(moq-mux)* resync TS audio instead of aborting the broadcast ([#2751](https://github.com/moq-dev/moq/pull/2751))

### Other

- *(deps)* bump cargo group and support mp4-atom 0.15 ([#2728](https://github.com/moq-dev/moq/pull/2728))

## [0.9.5](https://github.com/moq-dev/moq/compare/moq-mux-v0.9.4...moq-mux-v0.9.5) - 2026-08-07

### Added

- *(moq-mux)* add stateful per-frame fMP4 fragmenting
- *(moq-mux)* timescale control, and two duration fixes ([#2693](https://github.com/moq-dev/moq/pull/2693))

## [0.9.4](https://github.com/moq-dev/moq/compare/moq-mux-v0.9.3...moq-mux-v0.9.4) - 2026-08-06

### Added

- *(hang)* declare a configurable 30s retention on media tracks, and fix relayed cache misses ([#2615](https://github.com/moq-dev/moq/pull/2615))

### Fixed

- *(moq-mux)* make the synthesized fMP4 init playable on strict players ([#2679](https://github.com/moq-dev/moq/pull/2679))

## [0.9.3](https://github.com/moq-dev/moq/compare/moq-mux-v0.9.2...moq-mux-v0.9.3) - 2026-08-05

### Fixed

- *(hang)* reverse bits, not bytes, in the HEVC codec string ([#2658](https://github.com/moq-dev/moq/pull/2658))

### Other

- *(rs)* clean up pedantic clippy warnings ([#2621](https://github.com/moq-dev/moq/pull/2621))

## [0.9.2](https://github.com/moq-dev/moq/compare/moq-mux-v0.9.1...moq-mux-v0.9.2) - 2026-08-03

### Fixed

- *(loc)* adopt the draft-ietf-moq-loc-04 Timestamp code point, and align relay-hops with lite-06 ([#2581](https://github.com/moq-dev/moq/pull/2581))

### Other

- *(deps)* bump the cargo group with 2 updates ([#2591](https://github.com/moq-dev/moq/pull/2591))

## [0.9.1](https://github.com/moq-dev/moq/compare/moq-mux-v0.9.0...moq-mux-v0.9.1) - 2026-07-27

### Added

- *(mux)* expose catalog bitrate/jitter measurement as catalog::Estimator ([#2530](https://github.com/moq-dev/moq/pull/2530))

## [0.9.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.8.2...moq-mux-v0.9.0) - 2026-07-25

### Added

- *(moq-mux)* caller-driven audio grouping via dumb importers ([#2496](https://github.com/moq-dev/moq/pull/2496))

### Fixed

- *(opus)* propagate pre-skip and encoder controls ([#2492](https://github.com/moq-dev/moq/pull/2492))

## [0.8.2](https://github.com/moq-dev/moq/compare/moq-mux-v0.8.1...moq-mux-v0.8.2) - 2026-07-24

### Added

- *(moq-mux,moq-boy)* mark discontinuities, and never time a sample across one ([#2475](https://github.com/moq-dev/moq/pull/2475))

## [0.8.1](https://github.com/moq-dev/moq/compare/moq-mux-v0.8.0...moq-mux-v0.8.1) - 2026-07-23

### Added

- *(mux)* hvc1 support for import::Track ([#2444](https://github.com/moq-dev/moq/pull/2444))

### Other

- *(rust)* pin the toolchain and correct the MSRV claims ([#2462](https://github.com/moq-dev/moq/pull/2462))

## [0.8.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.7.6...moq-mux-v0.8.0) - 2026-07-22

### Added

- *(mux)* preserve the TS service layer as opaque SI sections ([#2440](https://github.com/moq-dev/moq/pull/2440))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))
- *(mux)* emit timelines from the fMP4 CMAF passthrough importer ([#2428](https://github.com/moq-dev/moq/pull/2428))
- *(moq-mux)* emit per-frame fragments without waiting for a successor frame ([#2426](https://github.com/moq-dev/moq/pull/2426))

### Other

- *(mux)* cover the per-PID SI repetition interval ([#2447](https://github.com/moq-dev/moq/pull/2447))
- *(mux)* extract shared video-import catalog helper ([#2425](https://github.com/moq-dev/moq/pull/2425))
- [**breaking**] pre-bump API polish across the release batch ([#2423](https://github.com/moq-dev/moq/pull/2423))
- *(mux)* [**breaking**] unseal catalog renditions and make timelines explicit/shareable ([#2420](https://github.com/moq-dev/moq/pull/2420))
- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- Merge branch 'main' into dev

## [0.7.6](https://github.com/moq-dev/moq/compare/moq-mux-v0.7.5...moq-mux-v0.7.6) - 2026-07-18

### Fixed

- *(moq-mux)* strip edit lists from the CMAF init ([#2383](https://github.com/moq-dev/moq/pull/2383))

### Other

- *(moq-mux)* bump mp4-atom to 0.14 ([#2391](https://github.com/moq-dev/moq/pull/2391))

## [0.7.5](https://github.com/moq-dev/moq/compare/moq-mux-v0.7.4...moq-mux-v0.7.5) - 2026-07-16

### Added

- *(moq-mux)* cut(end) as the group boundary ([#2270](https://github.com/moq-dev/moq/pull/2270))

### Fixed

- *(moq-hls)* honor byte ranges and map changes ([#2271](https://github.com/moq-dev/moq/pull/2271))

## [0.7.4](https://github.com/moq-dev/moq/compare/moq-mux-v0.7.3...moq-mux-v0.7.4) - 2026-07-12

### Other

- split into snapshot/stream modules and expose JSON tracks through moq-ffi/libmoq ([#2196](https://github.com/moq-dev/moq/pull/2196))

## [0.7.3](https://github.com/moq-dev/moq/compare/moq-mux-v0.7.2...moq-mux-v0.7.3) - 2026-07-09

### Fixed

- *(moq-mux)* match per-rendition init track id to its fragments ([#2104](https://github.com/moq-dev/moq/pull/2104))

### Other

- Per-track timeline index for each media track ([#2109](https://github.com/moq-dev/moq/pull/2109))

## [0.7.2](https://github.com/moq-dev/moq/compare/moq-mux-v0.7.1...moq-mux-v0.7.2) - 2026-07-05

### Other

- *(deps)* bump the cargo group with 9 updates ([#2098](https://github.com/moq-dev/moq/pull/2098))
- [codex] fix AAC catalog description ([#2093](https://github.com/moq-dev/moq/pull/2093))

## [0.7.1](https://github.com/moq-dev/moq/compare/moq-mux-v0.7.0...moq-mux-v0.7.1) - 2026-07-04

### Added

- *(moq-rtmp,moq-mux)* enhanced-RTMP capsEx negotiation + multitrack ([#2068](https://github.com/moq-dev/moq/pull/2068))
- *(moq-mux)* add FLAC support (catalog + mp4/mkv import/export) ([#1969](https://github.com/moq-dev/moq/pull/1969))
- *(moq-mux,moq-gst)* MP3 support for MKV and GStreamer ([#1970](https://github.com/moq-dev/moq/pull/1970))
- *(moq-mux)* add MP3 audio support for FLV/RTMP ([#1967](https://github.com/moq-dev/moq/pull/1967))
- *(moq-mux)* Opus audio over MPEG-TS (import + export) ([#1965](https://github.com/moq-dev/moq/pull/1965))

### Fixed

- *(moq-mux)* treat H.264 recovery-point SEI as an open-GOP keyframe ([#2066](https://github.com/moq-dev/moq/pull/2066))

### Other

- [codex] fix FLV export DTS timestamps
- [codex] fix timestamp elapsed arithmetic ([#2051](https://github.com/moq-dev/moq/pull/2051))
- [codex] Future-proof moq-net metadata structs ([#2046](https://github.com/moq-dev/moq/pull/2046))
- [codex] fix fMP4 composition offsets ([#2015](https://github.com/moq-dev/moq/pull/2015))
- Add track name to the consumer skip/reset logs ([#2012](https://github.com/moq-dev/moq/pull/2012))

## [0.7.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.6.0...moq-mux-v0.7.0) - 2026-06-30

### Added

- *(moq-rtc)* add WebRTC (WHIP/WHEP) gateway ([#1916](https://github.com/moq-dev/moq/pull/1916))
- *(moq-srt)* bidirectional SRT/MPEG-TS gateway (+ timestamped ts::Export) ([#1915](https://github.com/moq-dev/moq/pull/1915))
- *(moq-rtmp)* RTMP/E-RTMP gateway + enhanced-RTMP FLV codecs on main ([#1914](https://github.com/moq-dev/moq/pull/1914))
- *(hang)* compressed catalog track (catalog.json.z) ([#1904](https://github.com/moq-dev/moq/pull/1904))
- *(json)* group-scoped DEFLATE compression with browser support ([#1897](https://github.com/moq-dev/moq/pull/1897))
- *(moq-mux)* clear consumer buffer when group timestamps rewind ([#1884](https://github.com/moq-dev/moq/pull/1884))

### Fixed

- *(moq-mux)* codec/container correctness fixes from #1918 review ([#1923](https://github.com/moq-dev/moq/pull/1923)) ([#1925](https://github.com/moq-dev/moq/pull/1925))

### Other

- drop redundant non_exhaustive on select builders ([#1944](https://github.com/moq-dev/moq/pull/1944))
- unify rendition selection behind select::Broadcast
- API cleanup before the semver bump ([#1941](https://github.com/moq-dev/moq/pull/1941))
- [codex] Route HLS CLI import through moq-hls ([#1939](https://github.com/moq-dev/moq/pull/1939))
- Fix fMP4 zero-duration samples ([#1933](https://github.com/moq-dev/moq/pull/1933))
- [codex] Backport moq-hls to main ([#1924](https://github.com/moq-dev/moq/pull/1924))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

### Changed

- Emit MSF catalogs at draft-ietf-moq-msf-01: `version` is the string `"draft-01"` and init data is carried via the root `initDataList` + per-track `initRef`. The MSF consumer still accepts draft-00 (numeric `version`, inline `initData`).

## [0.6.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.5.6...moq-mux-v0.6.0) - 2026-06-23

### Added

- *(catalog)* expose untyped catalog extensions via moq-ffi and libmoq ([#1886](https://github.com/moq-dev/moq/pull/1886))
- *(moq-mux)* generic verbatim MPEG-TS carriage (mpegts catalog section) ([#1815](https://github.com/moq-dev/moq/pull/1815))

### Fixed

- *(moq-mux)* author DTS for B-frame MPEG-TS export ([#1843](https://github.com/moq-dev/moq/pull/1843))
- *(moq-mux)* carry all distinct SPS/PPS/VPS through transmux, not just the last seen ([#1812](https://github.com/moq-dev/moq/pull/1812))

### Other

- move moq-cli's TS verbatim coverage into moq-mux ([#1879](https://github.com/moq-dev/moq/pull/1879))

## [0.5.6](https://github.com/moq-dev/moq/compare/moq-mux-v0.5.5...moq-mux-v0.5.6) - 2026-06-17

### Added

- *(json)* default delta_ratio to 8, count only delta bytes ([#1765](https://github.com/moq-dev/moq/pull/1765))

## [0.5.5](https://github.com/moq-dev/moq/compare/moq-mux-v0.5.4...moq-mux-v0.5.5) - 2026-06-16

### Fixed

- *(moq-mux)* confirm TS sync lock before trusting a candidate sync byte ([#1697](https://github.com/moq-dev/moq/pull/1697))

### Other

- Add FLV (Flash Video / RTMP) container support to moq-mux ([#1745](https://github.com/moq-dev/moq/pull/1745))
- Mux import with existing track ([#1684](https://github.com/moq-dev/moq/pull/1684))
- ingest and export legacy non-browser broadcast audio over MPEG-TS mp2, ac-3 & e-ac-3 ([#1701](https://github.com/moq-dev/moq/pull/1701))
- harden the scte35_inject fixture generator ([#1696](https://github.com/moq-dev/moq/pull/1696))
- *(moq-mux)* SIMD-accelerate MPEG-TS sync byte resync ([#1695](https://github.com/moq-dev/moq/pull/1695))
- SIMD start-code scanning via memchr ([#1694](https://github.com/moq-dev/moq/pull/1694))
- export SCTE-35 sections back to MPEG-TS ([#1685](https://github.com/moq-dev/moq/pull/1685))
- ingest SCTE-35 from MPEG-TS, and tolerate mid-stream joins ([#1617](https://github.com/moq-dev/moq/pull/1617))

## [0.5.4](https://github.com/moq-dev/moq/compare/moq-mux-v0.5.3...moq-mux-v0.5.4) - 2026-06-10

### Added

- *(moq-video,moq-cli)* webcam capture and publish ([#1669](https://github.com/moq-dev/moq/pull/1669))
- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))
- *(moq-json)* JSON Merge Patch snapshot/delta helper, route hang catalog through it ([#1655](https://github.com/moq-dev/moq/pull/1655))
- *(moq-mux)* add VP8 and VP9 codec modules (import + fMP4 export) ([#1625](https://github.com/moq-dev/moq/pull/1625))

### Fixed

- *(moq-mux)* keep catalog Consumer Clone + stable FramedFormat discriminants ([#1661](https://github.com/moq-dev/moq/pull/1661))

## [0.5.3](https://github.com/moq-dev/moq/compare/moq-mux-v0.5.2...moq-mux-v0.5.3) - 2026-06-03

### Other

- add MPEG-TS (transport stream) import and export ([#1587](https://github.com/moq-dev/moq/pull/1587))
- synthesize AAC esds in fMP4 export; guard MKV header race ([#1593](https://github.com/moq-dev/moq/pull/1593))

## [0.5.2](https://github.com/moq-dev/moq/compare/moq-mux-v0.5.1...moq-mux-v0.5.2) - 2026-05-30

### Other

- rename conducer crate to kio ([#1547](https://github.com/moq-dev/moq/pull/1547))
- add seek(sequence) on importers for explicit group boundaries ([#1515](https://github.com/moq-dev/moq/pull/1515))

## [0.5.1](https://github.com/moq-dev/moq/compare/moq-mux-v0.5.0...moq-mux-v0.5.1) - 2026-05-24

### Other

- *(rs)* add cargo-deny and resolve outstanding advisories ([#1486](https://github.com/moq-dev/moq/pull/1486))
- non_exhaustive VideoConfig/AudioConfig with constructors ([#1485](https://github.com/moq-dev/moq/pull/1485))

## [0.5.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.4.2...moq-mux-v0.5.0) - 2026-05-23

### Added

- Unified CMSF/Hang pipeline (cleanup of #1429) ([#1444](https://github.com/moq-dev/moq/pull/1444))

### Other

- Tag audio sources with a kind to drive Opus encoder settings ([#1446](https://github.com/moq-dev/moq/pull/1446))
- Add Low Overhead Container (LOC) frame format support ([#1388](https://github.com/moq-dev/moq/pull/1388))
- Add Matroska/WebM import and export support ([#1438](https://github.com/moq-dev/moq/pull/1438))
- Auto-detect catalog format from broadcast name extension ([#1394](https://github.com/moq-dev/moq/pull/1394))
- re-emit deprecated CMAF timescale/trackId in catalog ([#1440](https://github.com/moq-dev/moq/pull/1440))

## [0.4.2](https://github.com/moq-dev/moq/compare/moq-mux-v0.4.1...moq-mux-v0.4.2) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.4.1](https://github.com/moq-dev/moq/compare/moq-mux-v0.4.0...moq-mux-v0.4.1) - 2026-05-18

### Other

- send each frame as its own group ([#1414](https://github.com/moq-dev/moq/pull/1414))
- Expose track name and used/unused activity signals ([#1398](https://github.com/moq-dev/moq/pull/1398))
- Fix reading catalogs ([#1404](https://github.com/moq-dev/moq/pull/1404))

## [0.4.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.9...moq-mux-v0.4.0) - 2026-05-07

### Other

- moq-mux backport + dual-API cleanup ([#1341](https://github.com/moq-dev/moq/pull/1341))
- tighten public API surface and remove deprecated methods ([#1378](https://github.com/moq-dev/moq/pull/1378))
- Revert moq-lite FETCH/Subscription API changes ([#1372](https://github.com/moq-dev/moq/pull/1372))
- backport Subscription model API for FETCH readiness ([#1348](https://github.com/moq-dev/moq/pull/1348))
- hop-based clustering ([#1322](https://github.com/moq-dev/moq/pull/1322))

## [0.3.9](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.8...moq-mux-v0.3.9) - 2026-04-19

### Other

- Add README files for Rust crates ([#1284](https://github.com/moq-dev/moq/pull/1284))
- Clarify group delivery semantics with recv_group and next_group_ordered ([#1324](https://github.com/moq-dev/moq/pull/1324))

## [0.3.8](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.7...moq-mux-v0.3.8) - 2026-04-09

### Other

- Add import statistics tracking and reporting ([#1242](https://github.com/moq-dev/moq/pull/1242))
- Add bandwidth estimation for adaptive bitrate control ([#1208](https://github.com/moq-dev/moq/pull/1208))

## [0.3.7](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.6...moq-mux-v0.3.7) - 2026-04-07

### Other

- Add jitter tracking to video codecs and catalog metadata ([#1220](https://github.com/moq-dev/moq/pull/1220))

## [0.3.6](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.5...moq-mux-v0.3.6) - 2026-04-03

### Other

- Auto-pause emulation when no viewers are watching ([#1201](https://github.com/moq-dev/moq/pull/1201))
- release ([#1174](https://github.com/moq-dev/moq/pull/1174))

## [0.3.5](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.4...moq-mux-v0.3.5) - 2026-04-03

### Added

- *(moq-relay)* on-demand key resolution via --auth-keys ([#1188](https://github.com/moq-dev/moq/pull/1188))

### Other

- Bump moq-mux version from 0.3.4 to 0.3.5 ([#1198](https://github.com/moq-dev/moq/pull/1198))
- Add moq-relay release workflow and Nix cache configuration ([#1178](https://github.com/moq-dev/moq/pull/1178))
- Update dependencies including breaking changes ([#1175](https://github.com/moq-dev/moq/pull/1175))

## [0.3.4](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.3...moq-mux-v0.3.4) - 2026-03-26

### Other

- Use typed ordered::Consumer for video/audio in moq-ffi ([#1163](https://github.com/moq-dev/moq/pull/1163))

## [0.3.3](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.2...moq-mux-v0.3.3) - 2026-03-25

### Other

- Add Avc1 import for AVCC-formatted H.264 ([#1161](https://github.com/moq-dev/moq/pull/1161))
- Add generic ordered::Consumer/Producer to moq-mux ([#1155](https://github.com/moq-dev/moq/pull/1155))

## [0.3.2](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.1...moq-mux-v0.3.2) - 2026-03-13

### Other

- Uniffi async objects ([#1071](https://github.com/moq-dev/moq/pull/1071))
- Set MSRV to 1.85 (edition 2024) ([#1083](https://github.com/moq-dev/moq/pull/1083))

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.2.1...moq-mux-v0.3.0) - 2026-03-03

### Fixed

- mask AAC profile to 5 bits to prevent shift overflow ([#1028](https://github.com/moq-dev/moq/pull/1028))
- `Fmp4::init_audio()` doesn't populate description for AAC, causing downstream format mismatch ([#1024](https://github.com/moq-dev/moq/pull/1024))

### Other

- OrderedProducer API with max_group_duration ([#1007](https://github.com/moq-dev/moq/pull/1007))
- Tweak the API to revert some breaking changes. ([#1036](https://github.com/moq-dev/moq/pull/1036))
- Add typed initialization for Opus and AAC in moq-mux ([#1034](https://github.com/moq-dev/moq/pull/1034))
- Cache and re-insert parameter sets before keyframes in H.264/H.265 ([#1030](https://github.com/moq-dev/moq/pull/1030))
- Add moq-msf crate for MSF catalog support ([#993](https://github.com/moq-dev/moq/pull/993))
- Replace tokio::sync::watch with custom Producer/Subscriber ([#996](https://github.com/moq-dev/moq/pull/996))

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-mux-v0.2.0...moq-mux-v0.2.1) - 2026-02-12

### Other

- (AI) Add support for quiche to moq-native ([#928](https://github.com/moq-dev/moq/pull/928))

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.1.0...moq-mux-v0.2.0) - 2026-02-09

### Other

- AV1 decoder ([#920](https://github.com/moq-dev/moq/pull/920))
- Split Decoder into Decoder and StreamDecoder variants. ([#912](https://github.com/moq-dev/moq/pull/912))
- Use `moq` instead of `hang` for some crates ([#906](https://github.com/moq-dev/moq/pull/906))

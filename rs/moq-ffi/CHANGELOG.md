# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/moq-dev/moq/compare/moq-ffi-v0.4.0...moq-ffi-v0.4.1) - 2026-09-23

### Added

- *(ffi)* add a TrackDemand handle and expose demand() on JSON producers ([#3949](https://github.com/moq-dev/moq/pull/3949))

## [0.4.0](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.19...moq-ffi-v0.4.0) - 2026-09-23

### Added

- *(video)* fork v4l in-tree with checked-in V4L2 bindings ([#3867](https://github.com/moq-dev/moq/pull/3867))
- *(moq-ffi)* the decode pixel format reaches every uniffi binding ([#3892](https://github.com/moq-dev/moq/pull/3892))
- *(video)* [**breaking**] type the group configuration and make cut fallible ([#3876](https://github.com/moq-dev/moq/pull/3876))
- preserve video capture timing ([#3849](https://github.com/moq-dev/moq/pull/3849))
- *(ffi)* [**breaking**] scope announcement streams with patterns ([#3856](https://github.com/moq-dev/moq/pull/3856))
- *(net)* [**breaking**] simplify origin scoping ([#3804](https://github.com/moq-dev/moq/pull/3804))
- *(libmoq)* [**breaking**] finalize release API ([#3819](https://github.com/moq-dev/moq/pull/3819))
- *(hang)* [**breaking**] unify catalog APIs ([#3813](https://github.com/moq-dev/moq/pull/3813))
- *(net)* [**breaking**] slim the moq-net public surface ([#3779](https://github.com/moq-dev/moq/pull/3779))
- *(net)* [**breaking**] announce prefixes on every wire; consumers read paths ([#3770](https://github.com/moq-dev/moq/pull/3770))
- *(net)* [**breaking**] fold the session placeholders into PROTOCOL_VIOLATION ([#3755](https://github.com/moq-dev/moq/pull/3755))
- [**breaking**] name every rate estimate estimated_*_rate
- *(net)* [**breaking**] register the four placeholder stream codes
- *(net)* [**breaking**] rank anonymous hop chains last ([#3724](https://github.com/moq-dev/moq/pull/3724))
- [**breaking**] borrow publisher finish so abort can still run ([#3714](https://github.com/moq-dev/moq/pull/3714))
- *(net)* [**breaking**] one name per announce, request, and origin config concept ([#3725](https://github.com/moq-dev/moq/pull/3725))
- *(json)* [**breaking**] Config means the same thing in json and binary ([#3718](https://github.com/moq-dev/moq/pull/3718))
- *(ffi)* [**breaking**] drop MoqCancel; Go cancels through context.Context ([#3723](https://github.com/moq-dev/moq/pull/3723))
- *(ffi)* [**breaking**] use microseconds and matching origin verbs
- *(tokio)* [**breaking**] rename ConnectionStatsReader to connection::Monitor ([#3705](https://github.com/moq-dev/moq/pull/3705))
- *(ffi)* [**breaking**] carry the route cold cost through MoqRoute ([#3669](https://github.com/moq-dev/moq/pull/3669))
- *(ffi)* [**breaking**] replace MoqAudioCodec enum with opus() object ([#3671](https://github.com/moq-dev/moq/pull/3671))
- *(net)* [**breaking**] make reader group and frame limits explicit ([#3647](https://github.com/moq-dev/moq/pull/3647))
- *(net)* [**breaking**] advertise wildcard routes with Pattern events ([#3649](https://github.com/moq-dev/moq/pull/3649))
- *(ffi)* [**breaking**] fail configuration setters that cannot apply ([#3642](https://github.com/moq-dev/moq/pull/3642))
- *(ffi)* [**breaking**] preserve session and stream protocol error details ([#3615](https://github.com/moq-dev/moq/pull/3615))
- *(ffi)* [**breaking**] align create_broadcast, announce, and dynamic across bindings ([#3577](https://github.com/moq-dev/moq/pull/3577))
- *(ffi)* [**breaking**] follow the connection's bandwidth share ([#3622](https://github.com/moq-dev/moq/pull/3622))
- *(moq-ffi)* expose the reconnect epoch and QUIC stream cap ([#3627](https://github.com/moq-dev/moq/pull/3627))
- *(hang)* [**breaking**] empty frames close the previous frame's duration ([#3575](https://github.com/moq-dev/moq/pull/3575))

### Fixed

- *(net)* announce local broadcasts on origin cursors ([#3928](https://github.com/moq-dev/moq/pull/3928))
- *(moq-ffi)* cancel waits for the listening socket to close ([#3914](https://github.com/moq-dev/moq/pull/3914))
- *(ci)* repair nightly and meta-review failures ([#3799](https://github.com/moq-dev/moq/pull/3799))
- *(ffi)* stop the runtime thread at Python exit so it never dies mid-callback ([#3766](https://github.com/moq-dev/moq/pull/3766))
- *(ffi)* keep the first-frame cursor across empty groups and cancel ([#3641](https://github.com/moq-dev/moq/pull/3641))
- *(json)* [**breaking**] fallible modify(), abort the track when a dropped edit fails ([#3644](https://github.com/moq-dev/moq/pull/3644))
- *(ffi)* keep the lane guard alive for the whole read ([#3651](https://github.com/moq-dev/moq/pull/3651))
- *(ffi)* let group and datagram reads progress independently ([#3645](https://github.com/moq-dev/moq/pull/3645))
- *(moq-mux)* [**breaking**] make an fMP4 export fragment a group, on every track ([#3573](https://github.com/moq-dev/moq/pull/3573))

### Other

- *(moq-ffi)* move the bindgen CLI out of the library build ([#3908](https://github.com/moq-dev/moq/pull/3908))
- *(net)* routed_broadcast waits on the watch alone ([#3901](https://github.com/moq-dev/moq/pull/3901))
- *(rs)* read constant-size chunks with as_chunks ([#3899](https://github.com/moq-dev/moq/pull/3899))
- *(video)* [**breaking**] separate decoder output from subscription policy ([#3875](https://github.com/moq-dev/moq/pull/3875))
- *(audio)* [**breaking**] separate configuration contracts ([#3843](https://github.com/moq-dev/moq/pull/3843))
- *(audio)* [**breaking**] expose demand without track authority ([#3842](https://github.com/moq-dev/moq/pull/3842))
- Unify mux track and rendition ownership ([#3857](https://github.com/moq-dev/moq/pull/3857))
- Make media backends optional ([#3839](https://github.com/moq-dev/moq/pull/3839))
- *(video)* [**breaking**] type frame conversions ([#3846](https://github.com/moq-dev/moq/pull/3846))
- *(mux)* [**breaking**] share media rate policy ([#3840](https://github.com/moq-dev/moq/pull/3840))
- *(just)* consolidate full-suite actions ([#3823](https://github.com/moq-dev/moq/pull/3823))
- *(net)* [**breaking**] name path roles without new types ([#3826](https://github.com/moq-dev/moq/pull/3826))
- *(net)* [**breaking**] return the next deadline from driver polls ([#3828](https://github.com/moq-dev/moq/pull/3828))
- *(net)* [**breaking**] drive time and cache cleanup explicitly ([#3825](https://github.com/moq-dev/moq/pull/3825))
- *(net)* expose route cost fields ([#3802](https://github.com/moq-dev/moq/pull/3802))
- *(tokio)* make API shapes type-safe ([#3816](https://github.com/moq-dev/moq/pull/3816))
- Merge origin/main into dev
- Merge remote-tracking branch 'origin/dev' into merge-main-into-dev-20260914
- merge main into dev
- merge main into dev

### Removed

- `MoqCancel` and the trailing `cancel` argument on blocking async methods. Bindings with
  native async cancellation never needed it; Go now cancels through `context.Context` on
  the generated call.

### Added

- `MoqError::Busy` when a configuration setter races an in-flight connect, listen, or accept.

### Fixed

- `TrackConsumer.read_frame` skips completed empty groups instead of returning
  EOF, keeps a group across a cancelled read so its first frame is not lost,
  and drops a group that errors so a later read can move on.

### Changed

- `MoqClient::set_tls_disable_verify(bool)` is `set_tls_verify(bool)`, so the
  argument's polarity matches every ergonomic wrapper.
- `MoqRequest::transport()` returns the closed `MoqTransport` enum instead of a string.
- Bare-integer durations are microseconds: `max_age_us` on decoder outputs,
  track info, and subscriptions; `MoqBackoff` is `initial_us` / `max_us` /
  `timeout_us`. `MoqSession::publish()` / `consume()` match `set_publish` /
  `set_consume`.
- `MoqAnnounced` is `MoqAnnounceConsumer`, `MoqAnnouncement` is
  `MoqAnnounceUpdate` with `prefix()` instead of `path()`; the returned covered
  prefix is relative to the prefix passed to `announced`. `MoqBroadcastRequest::abort`
  is `reject`, and `MoqOriginOptions` is `MoqOriginConfig`.

- [**breaking**] `MoqTrackProducer::finish` and `MoqGroupProducer::finish` keep the handle open so a
  later `abort` can still run. Broadcast, audio, video, and JSON producers still close on finish.
- Client, server, and pending-request configuration setters now return `Result` and
  apply or fail. They error with `Busy` while connect/listen/accept owns the handle
  and `Cancelled` after `cancel()`. Server bind/TLS is captured at `listen()` and
  those setters fail afterwards; request origin overrides fail with `AlreadyResponded`
  after accept/reject. `cert_fingerprints()` uses `Busy` instead of `Bind` when the
  server is in an accept/listen call.

- `MoqError` is no longer a flat error. `Protocol` carries a `MoqProtocolError` record
  (session or stream scope, the verbatim wire code, a known kind, and a message). Transport
  failures are `Transport`; local failures without a protocol code are `Internal`. Associated
  data on other variants is now visible to bindings instead of being flattened into the message.

- `publish_container_stream` takes a `MoqContainerFormat` instead of a `MoqContainerInit`, which
  carried leading bytes the stream importer discarded.

- `publish_media`, `publish_media_on_track`, and `publish_media_stream` split by media kind:
  `publish_audio`, `publish_video`, `publish_container`, `publish_audio_on_track`,
  `publish_video_on_track`, `publish_video_stream`, and `publish_container_stream`. Each takes only
  the fields its kind can honor, so a video hint on an audio track and a label on a container are no
  longer expressible. There is no audio stream variant: audio has no frame boundaries to infer.
- `MoqInit` splits into `MoqAudioInit`, `MoqVideoInit`, and `MoqContainerInit`, and the format is a
  typed `MoqAudioFormat` / `MoqVideoFormat` / `MoqContainerFormat` rather than a string.
- The raw encoder paths are `encode_audio` and `encode_video`, freeing `publish_audio` /
  `publish_video` for the bring-your-own-encoder path. C already called these `_raw`.
- `MoqAudioFormat` (the PCM sample layout) is now `MoqAudioSampleFormat`, matching the existing
  `MoqVideoPixelFormat`.
- A container gets its own `MoqContainerProducer` / `MoqContainerStreamProducer` rather than sharing
  `MoqMediaProducer`. `write` takes no timestamp, since a container carries its own timing and the
  shared type silently dropped it; and `name`/`used`/`unused` no longer have a container case to
  fail on.

### Added

- `MoqErrorScope`, `MoqProtocolKind`, and `MoqProtocolError` so every binding can read a
  peer's session or stream code without parsing a message.

- Publish and consume human-readable audio and video rendition labels.

### Changed

- `publish_media` and `publish_media_stream` reject a `MoqInit` label or video hint on a container
  format, and an audio format rejects a video hint, instead of silently dropping either.

## [0.3.19](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.18...moq-ffi-v0.3.19) - 2026-09-17

### Other

- update Cargo.lock dependencies

## [0.3.18](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.17...moq-ffi-v0.3.18) - 2026-09-13

### Other

- update Cargo.lock dependencies

## [0.3.17](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.16...moq-ffi-v0.3.17) - 2026-09-09

### Added

- *(audio,video)* compile the device, render, and VAAPI code by default ([#3353](https://github.com/moq-dev/moq/pull/3353))

### Fixed

- *(rs)* compile the lib tests at --no-default-features ([#3397](https://github.com/moq-dev/moq/pull/3397))

### Other

- make the agent guides minimal and situational ([#3469](https://github.com/moq-dev/moq/pull/3469))
- take every feature that needs a library or libclang at build time off the defaults ([#3464](https://github.com/moq-dev/moq/pull/3464))
- *(deps)* bump the cargo group across 1 directory with 5 updates ([#3395](https://github.com/moq-dev/moq/pull/3395))

## [0.3.16](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.15...moq-ffi-v0.3.16) - 2026-09-02

### Added

- *(moq-audio)* expose Opus DTX classification ([#3238](https://github.com/moq-dev/moq/pull/3238))
- *(dart)* add native Flutter bindings ([#3215](https://github.com/moq-dev/moq/pull/3215))

## [0.3.15](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.14...moq-ffi-v0.3.15) - 2026-09-01

### Added

- *(py)* add ergonomic route update iterator ([#3229](https://github.com/moq-dev/moq/pull/3229))

### Fixed

- *(ffi)* default optional record fields ([#3227](https://github.com/moq-dev/moq/pull/3227))

### Other

- *(rs)* point shared dependencies at [workspace.dependencies] ([#3098](https://github.com/moq-dev/moq/pull/3098))

## [0.3.14](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.13...moq-ffi-v0.3.14) - 2026-08-26

### Other

- updated the following local packages: moq-net, moq-mux, moq-native, moq-video, hang, moq-json, moq-audio

## [0.3.13](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.12...moq-ffi-v0.3.13) - 2026-08-24

### Added

- *(moq-ffi)* expose raw audio track demand ([#3020](https://github.com/moq-dev/moq/pull/3020))
- *(moq-ffi)* expose raw video track demand ([#3013](https://github.com/moq-dev/moq/pull/3013))
- *(audio)* decode AAC-LC ([#2968](https://github.com/moq-dev/moq/pull/2968))

### Fixed

- *(moq-ffi)* bump uniffi to 0.32 so Python copies payloads with memmove ([#2949](https://github.com/moq-dev/moq/pull/2949))

## [0.3.12](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.11...moq-ffi-v0.3.12) - 2026-08-20

### Added

- *(video)* give the bindings the NVIDIA codecs, and warn when Auto falls to software ([#2950](https://github.com/moq-dev/moq/pull/2950))
- *(moq-ffi)* compile for wasm32 ([#2911](https://github.com/moq-dev/moq/pull/2911))
- *(hang)* signal stalled video renditions ([#2865](https://github.com/moq-dev/moq/pull/2865))

## [0.3.11](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.10...moq-ffi-v0.3.11) - 2026-08-14

### Added

- *(bindings)* fetch and decode a retained media group ([#2827](https://github.com/moq-dev/moq/pull/2827))

### Fixed

- *(mux)* derive missing video geometry ([#2840](https://github.com/moq-dev/moq/pull/2840))
- *(net)* stop blocking connect on the initial announce set ([#2856](https://github.com/moq-dev/moq/pull/2856))

## [0.3.10](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.9...moq-ffi-v0.3.10) - 2026-08-13

### Added

- *(moq-video)* advertise the catalog rendition before the first keyframe ([#2768](https://github.com/moq-dev/moq/pull/2768))
- *(bindings)* expose incoming request path and query ([#2738](https://github.com/moq-dev/moq/pull/2738))

### Fixed

- *(release)* repair native package releases ([#2731](https://github.com/moq-dev/moq/pull/2731))
- *(moq-ffi)* support iOS simulator builds ([#2710](https://github.com/moq-dev/moq/pull/2710))

## [0.3.9](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.8...moq-ffi-v0.3.9) - 2026-08-07

### Other

- drop Intel macOS release targets ([#2715](https://github.com/moq-dev/moq/pull/2715))

## [0.3.8](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.7...moq-ffi-v0.3.8) - 2026-08-06

### Fixed

- *(libmoq)* unbreak the Linux C link list, and make VAAPI opt-in ([#2669](https://github.com/moq-dev/moq/pull/2669))

## [0.3.7](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.6...moq-ffi-v0.3.7) - 2026-08-05

### Added

- *(bindings)* publish raw video with a native encoder ([#2608](https://github.com/moq-dev/moq/pull/2608))

## [0.3.6](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.5...moq-ffi-v0.3.6) - 2026-08-03

### Other

- update Cargo.lock dependencies

## [0.3.5](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.4...moq-ffi-v0.3.5) - 2026-07-29

### Other

- updated the following local packages: moq-audio

## [0.3.4](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.3...moq-ffi-v0.3.4) - 2026-07-27

### Other

- updated the following local packages: moq-json, moq-audio

## [0.3.3](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.2...moq-ffi-v0.3.3) - 2026-07-25

### Added

- *(bindings)* expose shared video properties ([#2457](https://github.com/moq-dev/moq/pull/2457))

## [0.3.2](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.1...moq-ffi-v0.3.2) - 2026-07-24

### Fixed

- *(go-ffi)* install cross target std before building moq-ffi ([#2482](https://github.com/moq-dev/moq/pull/2482))

## [0.3.1](https://github.com/moq-dev/moq/compare/moq-ffi-v0.3.0...moq-ffi-v0.3.1) - 2026-07-23

### Fixed

- *(moq-ffi)* pin uniffi to 0.31 for uniffi-bindgen-go compatibility ([#2452](https://github.com/moq-dev/moq/pull/2452))

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.33...moq-ffi-v0.3.0) - 2026-07-22

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))

### Other

- Merge remote-tracking branch 'origin/main' into dev
- *(deps)* bump the cargo group with 2 updates ([#2409](https://github.com/moq-dev/moq/pull/2409))

### Added

- Expose explicit raw group sequences, known track ends, and raw group aborts
  across the generated bindings.

### Changed

- `abort` takes the application error code as a `u16`, matching the wire type,
  instead of an `i32` that was range-checked at runtime. The `InvalidErrorCode`
  error variant is gone with it: an out-of-range code no longer compiles.

## [0.2.33](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.32...moq-ffi-v0.2.33) - 2026-07-18

### Other

- *(deps)* bump the cargo group across 1 directory with 3 updates ([#2273](https://github.com/moq-dev/moq/pull/2273))

## [0.2.32](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.31...moq-ffi-v0.2.32) - 2026-07-17

### Fixed

- *(moq-ffi)* reject unsupported audio codecs ([#2331](https://github.com/moq-dev/moq/pull/2331))

## [0.2.31](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.30...moq-ffi-v0.2.31) - 2026-07-16

### Other

- update Cargo.lock dependencies

## [0.2.30](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.29...moq-ffi-v0.2.30) - 2026-07-15

### Added

- *(moq-ffi)* expose client mTLS certificate configuration ([#2256](https://github.com/moq-dev/moq/pull/2256))

## [0.2.29](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.28...moq-ffi-v0.2.29) - 2026-07-12

### Other

- split into snapshot/stream modules and expose JSON tracks through moq-ffi/libmoq ([#2196](https://github.com/moq-dev/moq/pull/2196))

## [0.2.28](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.27...moq-ffi-v0.2.28) - 2026-07-09

### Fixed

- *(moq-native)* compile for target_os="android" under jni 0.22 ([#2105](https://github.com/moq-dev/moq/pull/2105))

## [0.2.27](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.26...moq-ffi-v0.2.27) - 2026-07-05

### Other

- [codex] fix AAC catalog description ([#2093](https://github.com/moq-dev/moq/pull/2093))

## [0.2.26](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.25...moq-ffi-v0.2.26) - 2026-07-04

### Added

- *(moq-ffi)* expose TLS system root trust control ([#1978](https://github.com/moq-dev/moq/pull/1978))

### Fixed

- *(moq-native)* verify server certs with the OS platform verifier ([#1968](https://github.com/moq-dev/moq/pull/1968))

### Other

- [codex] Future-proof moq-net metadata structs ([#2046](https://github.com/moq-dev/moq/pull/2046))

## [0.2.25](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.24...moq-ffi-v0.2.25) - 2026-06-30

### Other

- API cleanup before the semver bump ([#1941](https://github.com/moq-dev/moq/pull/1941))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

## [0.2.23](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.22...moq-ffi-v0.2.23) - 2026-06-23

### Added

- *(catalog)* expose untyped catalog extensions via moq-ffi and libmoq ([#1886](https://github.com/moq-dev/moq/pull/1886))

## [0.2.22](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.21...moq-ffi-v0.2.22) - 2026-06-19

### Other

- update Cargo.lock dependencies

## [0.2.21](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.20...moq-ffi-v0.2.21) - 2026-06-16

### Added

- certificate pinning for native and browser clients ([#1698](https://github.com/moq-dev/moq/pull/1698))
- *(moq-ffi)* expose dynamic track requests ([#1674](https://github.com/moq-dev/moq/pull/1674))

### Fixed

- *(native)* surface terminal auth connect errors ([#1649](https://github.com/moq-dev/moq/pull/1649))

### Other

- Mux import with existing track ([#1684](https://github.com/moq-dev/moq/pull/1684))

## [0.2.20](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.19...moq-ffi-v0.2.20) - 2026-06-10

### Added

- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))

## [0.2.19](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.18...moq-ffi-v0.2.19) - 2026-06-03

### Other

- update Cargo.lock dependencies

## [0.2.18](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.17...moq-ffi-v0.2.18) - 2026-06-02

### Other

- shrink moq-ffi & libmoq staticlibs with LTO (unblocks the moq-go mirror push) ([#1577](https://github.com/moq-dev/moq/pull/1577))

## [0.2.17](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.16...moq-ffi-v0.2.17) - 2026-05-30

### Other

- route Android logs to logcat ([#1541](https://github.com/moq-dev/moq/pull/1541))

## [0.2.16](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.15...moq-ffi-v0.2.16) - 2026-05-30

### Other

- ship moq.h and linux staticlibs so the Go module builds for consumers ([#1549](https://github.com/moq-dev/moq/pull/1549))
- streaming media import + cross-language interop smoke test ([#1529](https://github.com/moq-dev/moq/pull/1529))
- re-export FFI, session.shutdown(); explicit Origin wiring ([#1526](https://github.com/moq-dev/moq/pull/1526))

## [0.2.15](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.14...moq-ffi-v0.2.15) - 2026-05-27

### Other

- moq-mux: add seek(sequence) on importers for explicit group boundaries ([#1515](https://github.com/moq-dev/moq/pull/1515))
- moq-net: add Lite05Wip version variant (unadvertised) ([#1518](https://github.com/moq-dev/moq/pull/1518))

## [0.2.14](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.13...moq-ffi-v0.2.14) - 2026-05-25

### Other

- ci(swift): decouple release manifest from dev Package.swift and gate publish on SPM resolve ([#1502](https://github.com/moq-dev/moq/pull/1502))

## [0.2.13](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.12...moq-ffi-v0.2.13) - 2026-05-24

### Added

- add moq-audio crate, raw-audio FFI, and rename moq-codec to moq-video ([#1484](https://github.com/moq-dev/moq/pull/1484))

## [0.2.12](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.11...moq-ffi-v0.2.12) - 2026-05-23

### Other

- Add Python MoQ server API with session acceptance and handshake ([#1417](https://github.com/moq-dev/moq/pull/1417))
- Tighten moq-ffi release pipeline ahead of first publish ([#1447](https://github.com/moq-dev/moq/pull/1447))
- Add Low Overhead Container (LOC) frame format support ([#1388](https://github.com/moq-dev/moq/pull/1388))
- re-emit deprecated CMAF timescale/trackId in catalog ([#1440](https://github.com/moq-dev/moq/pull/1440))
- Add Swift and Kotlin FFI wrappers with packaging and publishing ([#1432](https://github.com/moq-dev/moq/pull/1432))

## [0.2.11](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.10...moq-ffi-v0.2.11) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.2.10](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.9...moq-ffi-v0.2.10) - 2026-05-18

### Other

- Expose track name and used/unused activity signals ([#1398](https://github.com/moq-dev/moq/pull/1398))

## [0.2.8](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.7...moq-ffi-v0.2.8) - 2026-05-07

### Other

- moq-mux backport + dual-API cleanup ([#1341](https://github.com/moq-dev/moq/pull/1341))
- tighten public API surface and remove deprecated methods ([#1378](https://github.com/moq-dev/moq/pull/1378))
- Revert moq-lite FETCH/Subscription API changes ([#1372](https://github.com/moq-dev/moq/pull/1372))
- backport Subscription model API for FETCH readiness ([#1348](https://github.com/moq-dev/moq/pull/1348))
- hop-based clustering ([#1322](https://github.com/moq-dev/moq/pull/1322))

## [0.2.7](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.6...moq-ffi-v0.2.7) - 2026-04-19

### Other

- Adding  data (aka json) to the py_lib ([#1318](https://github.com/moq-dev/moq/pull/1318))

## [0.2.6](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.5...moq-ffi-v0.2.6) - 2026-04-17

### Other

- update Cargo.lock dependencies

## [0.2.5](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.4...moq-ffi-v0.2.5) - 2026-04-15

### Other

- update Cargo.lock dependencies

## [0.2.4](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.3...moq-ffi-v0.2.4) - 2026-04-11

### Other

- update Cargo.lock dependencies

## [0.2.3](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.2...moq-ffi-v0.2.3) - 2026-04-09

### Other

- Add bandwidth estimation for adaptive bitrate control ([#1208](https://github.com/moq-dev/moq/pull/1208))

## [0.2.2](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.1...moq-ffi-v0.2.2) - 2026-04-07

### Other

- update Cargo.lock dependencies

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.0...moq-ffi-v0.2.1) - 2026-04-03

### Other

- update Cargo.lock dependencies

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.6...moq-ffi-v0.2.0) - 2026-03-26

### Other

- Use typed ordered::Consumer for video/audio in moq-ffi ([#1163](https://github.com/moq-dev/moq/pull/1163))

## [0.1.6](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.5...moq-ffi-v0.1.6) - 2026-03-25

### Other

- update Cargo.lock dependencies

## [0.1.4](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.3...moq-ffi-v0.1.4) - 2026-03-18

### Other

- Fix FFI test panic strategy mismatch ([#1128](https://github.com/moq-dev/moq/pull/1128))
- Remove unused dev-dependencies and bump @moq/qmux ([#1126](https://github.com/moq-dev/moq/pull/1126))

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.2...moq-ffi-v0.1.3) - 2026-03-16

### Other

- Add FFI test for objects without tokio runtime ([#1112](https://github.com/moq-dev/moq/pull/1112))
- Fix MoqSession drop requiring tokio runtime ([#1109](https://github.com/moq-dev/moq/pull/1109))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/moq-ffi-v0.1.0) - 2026-03-13

### Other

- Publish moq-ffi just to trigger release-plz. ([#1094](https://github.com/moq-dev/moq/pull/1094))
- Uniffi async objects ([#1071](https://github.com/moq-dev/moq/pull/1071))

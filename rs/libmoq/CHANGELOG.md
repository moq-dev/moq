# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.3](https://github.com/moq-dev/moq/compare/libmoq-v0.6.2...libmoq-v0.6.3) - 2026-09-25

### Added

- *(libmoq)* accept sessions as a server ([#4046](https://github.com/moq-dev/moq/pull/4046))

### Fixed

- *(net)* a broadcast exists only while announced ([#4021](https://github.com/moq-dev/moq/pull/4021))

### Other

- *(bindings)* compile every binding doc sample against its wrapper ([#4049](https://github.com/moq-dev/moq/pull/4049))

## [0.6.2](https://github.com/moq-dev/moq/compare/libmoq-v0.6.1...libmoq-v0.6.2) - 2026-09-24

### Other

- rename in-repo smoke test to interop ([#3963](https://github.com/moq-dev/moq/pull/3963))

## [0.6.1](https://github.com/moq-dev/moq/compare/libmoq-v0.6.0...libmoq-v0.6.1) - 2026-09-23

### Other

- updated the following local packages: moq-json, moq-tokio, moq-video, hang, moq-mux, moq-audio

## [0.6.0](https://github.com/moq-dev/moq/compare/libmoq-v0.5.16...libmoq-v0.6.0) - 2026-09-23

### Added

- *(video)* [**breaking**] type the group configuration and make cut fallible ([#3876](https://github.com/moq-dev/moq/pull/3876))
- preserve video capture timing ([#3849](https://github.com/moq-dev/moq/pull/3849))
- *(ffi)* [**breaking**] scope announcement streams with patterns ([#3856](https://github.com/moq-dev/moq/pull/3856))
- *(moq-net)* add moq-transport draft-22 (moqt-22) ([#3858](https://github.com/moq-dev/moq/pull/3858))
- *(net)* [**breaking**] simplify origin scoping ([#3804](https://github.com/moq-dev/moq/pull/3804))
- *(libmoq)* [**breaking**] finalize release API ([#3819](https://github.com/moq-dev/moq/pull/3819))
- *(hang)* [**breaking**] unify catalog APIs ([#3813](https://github.com/moq-dev/moq/pull/3813))
- *(net)* [**breaking**] slim the moq-net public surface ([#3779](https://github.com/moq-dev/moq/pull/3779))
- *(net)* [**breaking**] announce prefixes on every wire; consumers read paths ([#3770](https://github.com/moq-dev/moq/pull/3770))
- *(libmoq)* track demand and dynamic track requests ([#3748](https://github.com/moq-dev/moq/pull/3748))
- *(tokio)* [**breaking**] settle moq-tokio names under their modules ([#3745](https://github.com/moq-dev/moq/pull/3745))
- *(net)* [**breaking**] fold the session placeholders into PROTOCOL_VIOLATION ([#3755](https://github.com/moq-dev/moq/pull/3755))
- [**breaking**] name every rate estimate estimated_*_rate
- *(net)* [**breaking**] rank anonymous hop chains last ([#3724](https://github.com/moq-dev/moq/pull/3724))
- [**breaking**] borrow publisher finish so abort can still run ([#3714](https://github.com/moq-dev/moq/pull/3714))
- *(net)* [**breaking**] one name per announce, request, and origin config concept ([#3725](https://github.com/moq-dev/moq/pull/3725))
- *(json)* [**breaking**] Config means the same thing in json and binary ([#3718](https://github.com/moq-dev/moq/pull/3718))
- *(ffi)* [**breaking**] use microseconds and matching origin verbs
- *(tokio)* [**breaking**] rename ConnectionStatsReader to connection::Monitor ([#3705](https://github.com/moq-dev/moq/pull/3705))
- *(ffi)* [**breaking**] carry the route cold cost through MoqRoute ([#3669](https://github.com/moq-dev/moq/pull/3669))
- *(libmoq)* [**breaking**] select decoded pixel format and size from C ([#3674](https://github.com/moq-dev/moq/pull/3674))
- *(net)* [**breaking**] make reader group and frame limits explicit ([#3647](https://github.com/moq-dev/moq/pull/3647))
- *(net)* [**breaking**] advertise wildcard routes with Pattern events ([#3649](https://github.com/moq-dev/moq/pull/3649))
- *(e2ee)* add the moq-e2ee-01 profile and shared vectors ([#3629](https://github.com/moq-dev/moq/pull/3629))
- *(ffi)* [**breaking**] preserve session and stream protocol error details ([#3615](https://github.com/moq-dev/moq/pull/3615))
- *(ffi)* [**breaking**] align create_broadcast, announce, and dynamic across bindings ([#3577](https://github.com/moq-dev/moq/pull/3577))
- *(moq-net)* [**breaking**] abort an oversized group instead of shedding its head ([#3585](https://github.com/moq-dev/moq/pull/3585))
- *(ffi)* [**breaking**] follow the connection's bandwidth share ([#3622](https://github.com/moq-dev/moq/pull/3622))
- *(hang)* [**breaking**] empty frames close the previous frame's duration ([#3575](https://github.com/moq-dev/moq/pull/3575))

### Fixed

- *(net)* announce local broadcasts on origin cursors ([#3928](https://github.com/moq-dev/moq/pull/3928))
- *(libmoq)* build on the noq backend like the rest of dev ([#3758](https://github.com/moq-dev/moq/pull/3758))
- *(json)* [**breaking**] fallible modify(), abort the track when a dropped edit fails ([#3644](https://github.com/moq-dev/moq/pull/3644))
- *(moq-mux)* [**breaking**] make an fMP4 export fragment a group, on every track ([#3573](https://github.com/moq-dev/moq/pull/3573))
- *(moq-mux)* catalog jitter is the publisher's maximum flush span ([#3513](https://github.com/moq-dev/moq/pull/3513))

### Other

- *(net)* routed_broadcast waits on the watch alone ([#3901](https://github.com/moq-dev/moq/pull/3901))
- *(video)* [**breaking**] separate decoder output from subscription policy ([#3875](https://github.com/moq-dev/moq/pull/3875))
- *(audio)* [**breaking**] separate configuration contracts ([#3843](https://github.com/moq-dev/moq/pull/3843))
- *(audio)* [**breaking**] expose demand without track authority ([#3842](https://github.com/moq-dev/moq/pull/3842))
- Unify mux track and rendition ownership ([#3857](https://github.com/moq-dev/moq/pull/3857))
- Make media backends optional ([#3839](https://github.com/moq-dev/moq/pull/3839))
- *(video)* [**breaking**] type frame conversions ([#3846](https://github.com/moq-dev/moq/pull/3846))
- *(mux)* [**breaking**] share media rate policy ([#3840](https://github.com/moq-dev/moq/pull/3840))
- *(net)* [**breaking**] name path roles without new types ([#3826](https://github.com/moq-dev/moq/pull/3826))
- *(quic)* [**breaking**] keep only the noq backend ([#3811](https://github.com/moq-dev/moq/pull/3811))
- *(net)* expose route cost fields ([#3802](https://github.com/moq-dev/moq/pull/3802))
- *(tokio)* make API shapes type-safe ([#3816](https://github.com/moq-dev/moq/pull/3816))
- Merge origin/main into dev
- Merge remote-tracking branch 'origin/dev' into merge-main-into-dev-20260914
- *(libmoq)* run the C decoder fixture outside cargo test ([#3691](https://github.com/moq-dev/moq/pull/3691))
- merge main into dev
- merge main into dev

### Added

- `moq_origin_create_broadcast`, `moq_publish_announce` / `_unannounce`, and
  `moq_origin_dynamic` split creation from exact-path and prefix advertisements.
- `moq_session_bandwidth`, `moq_bandwidth_reserve`, `moq_reservation_grant` / `_update` /
  `_close` expose the connection's send estimate to app-owned encoders.
- `moq_session_snapshot` samples connection statistics and the negotiated protocol together;
  `moq_connection_stats` includes `estimated_send_rate` and `estimated_receive_rate`.
- `moq_video_decoder_output` selects the decoded pixel format and dimensions.
- `moq_route` carries the advertised warm and cold path costs.
- `moq_audio_encoder_output.frame_duration_us` configures the Opus packet duration.
- Publish and consume human-readable audio and video rendition labels.
- The generated header defines every `MOQ_ERROR_*` return code, every enum used by a struct,
  and one `moq_status_callback` type for asynchronous registrars.
- Demand watchers: `moq_publish_track_demand`, `moq_publish_media_demand`,
  `moq_encode_video_demand`, and `moq_encode_audio_demand` report `MOQ_DEMAND_USED` /
  `MOQ_DEMAND_UNUSED` (a `moq_demand`) immediately and on every change, closed by
  `moq_publish_demand_cancel`.
- Track requests: `moq_publish_dynamic` serves subscriptions to undeclared tracks as
  `moq_track_request_*` handles (`name`, `accept`, `video`, `audio`, `abort`, `free`).
  Group requests: `moq_publish_track_dynamic` and `moq_track_request_dynamic` serve fetches
  of uncached groups as `moq_group_request_*` handles (`sequence`, `priority`, `frame_start`,
  `accept`, `abort`, `free`). `accept` positions the producer at `frame_start`. Both
  handlers stop with `moq_publish_dynamic_cancel`.
- `moq_error_protocol` fills a `moq_protocol_error` (scope, verbatim wire code, kind) for
  the last protocol failure on this thread. Do not parse `moq_error()` for that. Local
  `Unauthorized` still returns status -34; a session-scoped unauthorized protocol close is
  `Error::Moq` (-2) with this record.

### Changed

- Bare-integer durations are microseconds: `max_age_us` on decoder outputs,
  track info, subscriptions, and `moq_consume_video` / `moq_consume_audio`;
  every duration in `moq_client_config` is `_us`.
- The 41 `moq_client_*` setters are replaced by one zero-initializable `moq_client_config`.
- Asynchronous task shutdown is consistently `_cancel`; `moq_origin_consume_announced` is
  `moq_origin_announced_broadcast`. Registrars reject a NULL callback before retaining
  `user_data`.
- `moq_publish_media` splits into `moq_publish_audio`, `moq_publish_video`, and
  `moq_publish_container`, taking `moq_audio_init`, `moq_video_init`, and `moq_container_init`.
  Each carries only the fields its kind can honor, so a label on a container no longer compiles.
- `moq_announced` is `moq_announce_update` with `prefix` / `prefix_len` instead
  of `path` / `path_len`. The prefix is relative to the requested announcements
  scope. `moq_broadcast_request_abort` is `moq_broadcast_request_reject`; `_free`
  is unchanged.
- Formats are enums (`moq_audio_format`, `moq_video_format`, `moq_container_format`) rather than
  strings. As with `moq_audio_sample_format`, the struct field is a `u32` and an out-of-range code
  is rejected, since matching an invalid discriminant as a Rust enum would be undefined behavior.
- A container has its own handle space and `moq_publish_container_write` / `_cut` / `_seek` /
  `_finish`. `write` takes no timestamp: a container carries its tracks' timing itself, and
  `moq_publish_media_frame` used to accept one and drop it. A handle from one family is rejected by
  the other's calls.
- The raw encoder entry points are `moq_encode_audio*` and `moq_encode_video*`, matching their
  existing `moq_video_encoder_frame` naming and freeing `moq_publish_audio` / `moq_publish_video`
  for the encoded path.
- `moq_audio_format` (the PCM sample layout) is now `moq_audio_sample_format`, matching
  `moq_video_pixel_format`.

## [0.5.16](https://github.com/moq-dev/moq/compare/libmoq-v0.5.15...libmoq-v0.5.16) - 2026-09-17

### Other

- updated the following local packages: moq-net, moq-mux, moq-native, hang, moq-json, moq-loc, moq-audio, moq-video

## [0.5.15](https://github.com/moq-dev/moq/compare/libmoq-v0.5.14...libmoq-v0.5.15) - 2026-09-13

### Added

- *(moq-net)* add moq-transport draft-21 (moqt-21) ([#3574](https://github.com/moq-dev/moq/pull/3574))

### Fixed

- *(moq-video,moq-audio)* open a decoder at the live edge ([#3565](https://github.com/moq-dev/moq/pull/3565))

### Other

- reach Cargo through mbx's shim and delete RUST_CARGO ([#3553](https://github.com/moq-dev/moq/pull/3553))

## [0.5.14](https://github.com/moq-dev/moq/compare/libmoq-v0.5.13...libmoq-v0.5.14) - 2026-09-09

### Added

- *(obs)* add connection stats and encoding controls to the MoQ dock ([#3453](https://github.com/moq-dev/moq/pull/3453))
- *(audio,video)* compile the device, render, and VAAPI code by default ([#3353](https://github.com/moq-dev/moq/pull/3353))

### Fixed

- *(native)* default noq and iroh to BBRv3 now that noq 1.2.0 lands the loss fix ([#3536](https://github.com/moq-dev/moq/pull/3536))

### Other

- take every feature that needs a library or libclang at build time off the defaults ([#3464](https://github.com/moq-dev/moq/pull/3464))

## [0.5.13](https://github.com/moq-dev/moq/compare/libmoq-v0.5.12...libmoq-v0.5.13) - 2026-09-02

### Added

- *(moq-audio)* expose Opus DTX classification ([#3238](https://github.com/moq-dev/moq/pull/3238))

## [0.5.12](https://github.com/moq-dev/moq/compare/libmoq-v0.5.11...libmoq-v0.5.12) - 2026-09-01

### Added

- *(moq-net)* add moq-transport draft-20 (moqt-20) ([#3255](https://github.com/moq-dev/moq/pull/3255))

### Other

- restore Swatinem Rust cache ([#3273](https://github.com/moq-dev/moq/pull/3273))
- try mr boxington cache ([#3175](https://github.com/moq-dev/moq/pull/3175))
- *(rs)* point shared dependencies at [workspace.dependencies] ([#3098](https://github.com/moq-dev/moq/pull/3098))

## [0.5.11](https://github.com/moq-dev/moq/compare/libmoq-v0.5.10...libmoq-v0.5.11) - 2026-08-26

### Other

- updated the following local packages: moq-net, moq-mux, moq-native, moq-video, hang, moq-json, moq-loc, moq-audio

## [0.5.10](https://github.com/moq-dev/moq/compare/libmoq-v0.5.9...libmoq-v0.5.10) - 2026-08-24

### Added

- *(audio)* decode AAC-LC ([#2968](https://github.com/moq-dev/moq/pull/2968))

## [0.5.9](https://github.com/moq-dev/moq/compare/libmoq-v0.5.8...libmoq-v0.5.9) - 2026-08-20

### Added

- *(video)* give the bindings the NVIDIA codecs, and warn when Auto falls to software ([#2950](https://github.com/moq-dev/moq/pull/2950))
- *(hang)* signal stalled video renditions ([#2865](https://github.com/moq-dev/moq/pull/2865))

## [0.5.8](https://github.com/moq-dev/moq/compare/libmoq-v0.5.7...libmoq-v0.5.8) - 2026-08-14

### Added

- *(libmoq)* declare the catalog container for manually authored renditions ([#2805](https://github.com/moq-dev/moq/pull/2805))

## [0.5.7](https://github.com/moq-dev/moq/compare/libmoq-v0.5.6...libmoq-v0.5.7) - 2026-08-13

### Added

- *(native)* start dialing before the AAAA answer lands ([#2749](https://github.com/moq-dev/moq/pull/2749))
- *(moq-video)* advertise the catalog rendition before the first keyframe ([#2768](https://github.com/moq-dev/moq/pull/2768))

## [0.5.6](https://github.com/moq-dev/moq/compare/libmoq-v0.5.5...libmoq-v0.5.6) - 2026-08-07

### Added

- *(libmoq)* add a client config handle, and advanced settings to the OBS plugin ([#2650](https://github.com/moq-dev/moq/pull/2650))

### Other

- drop Intel macOS release targets ([#2715](https://github.com/moq-dev/moq/pull/2715))

## [0.5.5](https://github.com/moq-dev/moq/compare/libmoq-v0.5.4...libmoq-v0.5.5) - 2026-08-06

### Fixed

- *(libmoq)* stop holding the global locks across a video encode ([#2663](https://github.com/moq-dev/moq/pull/2663))
- *(libmoq)* fix the generated CMake package config for Windows and installs ([#2671](https://github.com/moq-dev/moq/pull/2671))
- *(libmoq)* unbreak the Linux C link list, and make VAAPI opt-in ([#2669](https://github.com/moq-dev/moq/pull/2669))

## [0.5.4](https://github.com/moq-dev/moq/compare/libmoq-v0.5.3...libmoq-v0.5.4) - 2026-08-05

### Added

- *(bindings)* publish raw video with a native encoder ([#2608](https://github.com/moq-dev/moq/pull/2608))

### Fixed

- *(moq-video)* reject unrepresentable sizes, and unbreak the macOS build ([#2648](https://github.com/moq-dev/moq/pull/2648))

## [0.5.3](https://github.com/moq-dev/moq/compare/libmoq-v0.5.2...libmoq-v0.5.3) - 2026-08-03

### Other

- updated the following local packages: moq-video

## [0.5.2](https://github.com/moq-dev/moq/compare/libmoq-v0.5.1...libmoq-v0.5.2) - 2026-07-29

### Other

- updated the following local packages: moq-audio, moq-video

## [0.5.1](https://github.com/moq-dev/moq/compare/libmoq-v0.5.0...libmoq-v0.5.1) - 2026-07-27

### Other

- updated the following local packages: moq-json, moq-audio

## [0.5.0](https://github.com/moq-dev/moq/compare/libmoq-v0.4.2...libmoq-v0.5.0) - 2026-07-25

### Added

- *(bindings)* expose shared video properties ([#2457](https://github.com/moq-dev/moq/pull/2457))

### Other

- *(moq-video)* [**breaking**] one raw Frame type, and carry timestamps through encode ([#2503](https://github.com/moq-dev/moq/pull/2503))

## [0.4.2](https://github.com/moq-dev/moq/compare/libmoq-v0.4.1...libmoq-v0.4.2) - 2026-07-24

### Other

- updated the following local packages: moq-mux, moq-audio, moq-video

## [0.4.1](https://github.com/moq-dev/moq/compare/libmoq-v0.4.0...libmoq-v0.4.1) - 2026-07-23

### Other

- updated the following local packages: moq-json, moq-audio, moq-video

## [0.4.0](https://github.com/moq-dev/moq/compare/libmoq-v0.3.14...libmoq-v0.4.0) - 2026-07-22

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))

### Other

- [**breaking**] pre-bump API polish across the release batch ([#2423](https://github.com/moq-dev/moq/pull/2423))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- Merge branch 'main' into dev

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

## [0.3.14](https://github.com/moq-dev/moq/compare/libmoq-v0.3.13...libmoq-v0.3.14) - 2026-07-18

### Fixed

- *(libmoq)* write moq.pc under a profile-scoped path so debug/release don't collide ([#2379](https://github.com/moq-dev/moq/pull/2379))

## [0.3.13](https://github.com/moq-dev/moq/compare/libmoq-v0.3.12...libmoq-v0.3.13) - 2026-07-16

### Other

- updated the following local packages: moq-audio

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

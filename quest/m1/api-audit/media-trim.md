# [S] Media surface trim

## Goal

Media crate and package items added since release have a consumer or are
private, and Rust and JS media APIs name the same thing the same way.

## Plan

Decided 2026-10-22 (API audit). Make `pub(crate)` or delete:

- `fmp4::Import::dropped_metadata` and the FLV one.
- moq-audio `Applied` and `preset()`; settings mix `with_preset` with `pub`
  fields, and moq-ffi's audio config accepts preset plus duration only to
  refuse it at runtime. moq-video `Applied`, `Encoder::applied`,
  `Sink::applied`.
- moq-video capture `Control`/`Driver` (only `publish_capture` is used),
  `DmaBuf::new`, `DmaBufLayout` (until
  [OBS Linux GPU](/quest/m3/obs-linux-gpu.md)).
- moq-hls `pub mod master`, range playlists, and `history`: the CLI never
  sets history, so `?start=` always answers 416 while `doc/bin/hls.md`
  documents it. Remove the docs with it; see
  [history floor](/quest/m1/archive/history-floor.md).
- moq-json `window::Checkpoint`, `Encoder::resume`, `Producer::{resume, cut}`
  and `moq_mux::timeline::Producer::resume`, reached only by tests; the
  archive consumer does not use them.
- hang `Catalog::from_value`, `Section`/`deserialize_section`.
- The `@moq/loc` `Producer` (test-only, no Rust twin) and the JS
  `Container.Consumer` `maxAge` guard.

Make public for a consumer: moq-hls `Route`/`Resource` out of the `server`
feature, with `segment`/`init` taking the parsed id, so moq.pro's edge stops
copying `Route::parse` without its encoded-`/` refusal.

Naming:

- moq-audio `encode::Settings` becomes `Config` like video and decode; the
  two `encode::Options` that mean different things get distinct names; audio
  `Kind::Named` (a codec) goes, since video's names a backend.
- `hang::catalog::Range` (colour) and `moq_video::color::Range` duplicate
  each other; keep one or add `From`.
- moq-json `pop` and `mutate` take arguments in the same order in Rust and
  JS.
- JS publishers stamp the catalog `version` by hand; export one encode.
- `playback::Observer::finish(&self)` plus `finished()` forces polling;
  return a future.

Public API: removals and renames across media crates. Wire: none.

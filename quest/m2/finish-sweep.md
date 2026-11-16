# [M] Finishing an importer or the video producer returns a Finished handle

## Goal

Every media producer finishes the same way. The ten codec importers in
`rs/moq-mux/src/codec`, the two import front doors that wrap them
(`moq_mux::import::Track` and `TrackStream` in
`rs/moq-mux/src/import/track.rs`), and `moq_video::encode::Producer` take
`finish(self)` and return a `Finished` handle that keeps the track and its
catalog rendition published and can still `abort`. After `finish`, nothing
can publish another frame, because nothing holds the producer. The bindings
and every caller in the tree follow.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): this
completes the 2026-11-07 decision that
[moq-dev/moq#8395](https://github.com/moq-dev/moq/pull/8395) carried out
("finishing a media producer returns a Finished handle", settled in m2
before the cut). The audit found the sweep stopped short; no new decision
was needed, and none was recorded for leaving these out.

Facts found by the audit:

- #8395 converted the moq-mux container producers
  (`container::Producer::finish(self) -> Result<Finished<C, R>>`),
  moq-json, and
  `moq_audio::encode::Producer::finish(self) -> Result<Finished, Error>`.
  The container importers (`fmp4`, `flv`, `mkv`, `ts`) consume and return
  a `Finished` too.
- The video twin still borrows:
  `rs/moq-video/src/encode/producer.rs`,
  `pub fn finish(&mut self) -> Result<(), Error>`, documented as "borrows
  rather than consumes, so a later abort can still run after a successful
  finish". That is the case the `Finished` handle exists for, and after
  `finish`, `publish` is still callable.
- Ten codec importers still take `finish(&mut self) -> Result<()>`:
  `aac`, `av1`, `flac`, `h264`, `h265`, `legacy`, `mp3`, `opus`, `vp8`, and
  `vp9` under `rs/moq-mux/src/codec` (re-run
  `grep -rn 'pub fn finish(&mut self)' rs/moq-mux/src/codec rs/moq-mux/src/import`).
  Each ends in `self.track.end_with(None)` on a `container::Producer` that
  already has the consuming `finish`.
- `rs/AGENTS.md`: "Terminal operations consume `self`".

For the PR:

- Importers first: each `finish(self)` returns the `Finished` of the
  container producer it wraps (or a small `Finished` of its own exposing
  `name`, `track`, and `abort`, if the wrapped type should stay private).
  One shape for all ten; do not invent a per-codec handle.
- `import::Track` and `TrackStream` then return a `Finished` over
  whichever importer they hold. Their comment "the import stays, keeping
  the finished track and its rendition published" becomes the handle's
  job.
- `moq_video::encode::Producer::finish(self) -> Result<Finished, Error>`,
  with `Finished::abort`, matching the audio producer line for line. This
  type is being renamed by
  [the frame-taking video producer](/quest/m2/video-producer.md); whichever
  lands second merges main and keeps both changes.
- Callers hold these behind locks in moq-ffi and moq-c
  (`rs/moq-ffi/src/{video,media,audio}.rs`, `rs/moq-c/src/{video,publish}.rs`):
  move the producer out of its slot on finish and store the `Finished`, as
  the audio encoder bindings already do. moq-cli, moq-gst, moq-rtsp, and
  moq-boy follow. The bindings' own API does not change shape.
- Add a `compile_fail` doctest per type, as `container::Finished` has, that
  writing after finish does not compile.
- Leave the internal `finish(&mut self)` helpers that are not producers
  (`ts::demux`, `ts::video`, `mkv::ebml`, `ts::stats`).

Public API: breaking for Rust: `finish` on the ten `moq_mux::codec`
importers, `moq_mux::import::{Track, TrackStream}`, and
`moq_video::encode::Producer`. Wire: none.

## Related

- [The frame-taking video producer](/quest/m2/video-producer.md) - renames the video type this changes
- [TS import reports typed outcomes](/quest/m2/ts-typed-outcomes.md) - changes the TS importers' `finish` error type

# [S] Transcoder mirrors clockless source catalogs and survives clock changes

## Goal

`moq-transcode` transcodes source broadcasts whose catalog omits `sync.clock` without inventing a synthetic host wall clock or crashing, and handles a post-anchor source `sync.clock` change without killing `Transcoder::run`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the transcode audit): ✅ allow unclocked derived catalogs mirroring `source.sync.clock == None`, and handle post-anchor clock changes without crashing (rejected: rejecting clockless sources with a clean error, which breaks valid publishers omitting `sync.clock`; leaving `derived`'s synthetic host wall clock in place when the source has `None`, which advertises a fabricated wall-clock mapping that disagrees with the source's raw timestamps).

Root cause: `rs/moq-transcode/src/catalog.rs` sets `out.sync.clock = source.sync.clock;` unconditionally, while `moq_mux::catalog::Producer::new` initializes with a host wall clock and `adopt()` (`rs/moq-mux/src/catalog/producer.rs`) returns `UnmappableTimestamp` whenever `sync.clock` is cleared to `None` or changed after anchoring.

Scope:
- Add an additive unclocked mode on `moq_mux::catalog::Producer` (or allow unanchored `None`) so `moq-transcode` mirrors `None` verbatim when the source carries no `sync.clock`.
- On a post-anchor `source.sync.clock` change, handle it cleanly (warn and keep the anchored clock, or cleanly re-anchor/restart the derivative) rather than failing `guard.commit()` and terminating `Transcoder::run`.
- Add unit and integration tests covering `source.sync.clock == None`, initial `Some(clock)` adoption, and a mid-stream `sync.clock` change.

Public API: additive only (no breaking change to `moq-mux` or `moq-transcode`).

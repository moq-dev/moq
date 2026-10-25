# [S] A malformed MKV track fails the import instead of vanishing

## Goal

The MKV importer fails loud on a malformed track, a duplicate track
number, or an invalid timestamp, instead of publishing a catalog that
silently lacks the track or carries collapsed timestamps.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670, source-inferred.

- `handle_tracks` (`rs/moq-mux/src/container/mkv/import.rs:338`) logs and
  skips every `add_track` error except an allowlist (`InvalidSubtitle`,
  `InvalidAspect`, `ValueOutOfRange`). `InvalidColour`
  ([moq-dev/moq#5846](https://github.com/moq-dev/moq/pull/5846)), a malformed
  avcC/hvcC, or a missing AAC CodecPrivate drops the video track and the
  catalog goes out audio-only. Every validation fix has had to extend the
  allowlist by hand ([moq-dev/moq#6532](https://github.com/moq-dev/moq/pull/6532)).
  Return `Ok` only for an unsupported role, decided where the error is
  raised, and propagate everything else, an unsupported codec included;
  delete the allowlist.
- A duplicate `TrackNumber` (`:452`) replaces the earlier track. Refuse it.
- `cluster_timestamp as i64 + block.timestamp` (`:533`) can overflow (a
  wrap in release builds, misreported as `NegativeBlockTimestamp`), and
  `TimestampScale = 0` (`:289`) collapses every timestamp to 0. Use
  `checked_add_signed` and refuse a zero scale.
- While in moq-mux codecs: `codec/vp9/mod.rs:140` computes
  `(r.read(16)? + 1) as u16`, so a 65536 dimension becomes 0 in the catalog.
  Use `u32`, like the catalog field.

Decided 2026-10-25 (maintainer): reject an unsupported codec everywhere, as
`rs/moq-mux/AGENTS.md` says. MKV and FLV (`flv/import.rs` about lines 215,
302, 359, 431) stop dropping such tracks and fail the import with a typed
Error.

Public API: none. Wire: none.

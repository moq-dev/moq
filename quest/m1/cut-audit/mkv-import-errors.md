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
  Return `Ok` only for an unsupported role or codec, decided where the error
  is raised, and propagate everything else; delete the allowlist.
- A duplicate `TrackNumber` (`:452`) replaces the earlier track. Refuse it.
- `cluster_timestamp as i64 + block.timestamp` (`:533`) can overflow (a
  wrap in release builds, misreported as `NegativeBlockTimestamp`), and
  `TimestampScale = 0` (`:289`) collapses every timestamp to 0. Use
  `checked_add_signed` and refuse a zero scale.
- While in moq-mux codecs: `codec/vp9/mod.rs:140` computes
  `(r.read(16)? + 1) as u16`, so a 65536 dimension becomes 0 in the catalog.
  Use `u32`, like the catalog field.

Decision for the maintainer: MKV and FLV (`flv/import.rs` about lines 215,
302, 359, 431) drop unsupported codecs by design, which contradicts
`rs/moq-mux/AGENTS.md` ("Reject an unsupported codec ... with a typed
Error"). Recommendation: keep dropping unsupported codecs in container
imports (a file with a subtitle or an exotic audio track should still
import its video) and ask the maintainer to scope that rule to codec
importers. Leave it under `## Decisions needed` if unanswered.

Public API: none. Wire: none.

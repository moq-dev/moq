# [M] fMP4 export keeps every track it promised in moov

## Goal

A live `moq export fmp4` recording is always readable and starts with all of
its media. Audio published before the first video keyframe is written, and a
rendition that leaves and returns after the init reuses its moov track instead
of writing an id the moov never declared.

## Plan

In `rs/moq-mux/src/container/fmp4/export.rs`, a ready track parks on one
pending frame until the init is ready. For Annex-B H.264 and H.265 that is the
first keyframe, so audio falls a full max-age budget behind and its groups are
skipped. After the init, `update_catalog` gives a returning name a fresh
`max(id) + 1` track id that is not in the moov, and ffprobe rejects the file.

Decided (2026-10-04), one quest because both are the export's track lifecycle:

- Before the init, keep draining every header-ready track into its buffer and
  flush the buffers after the moov.
- Once the moov is out, the track set is frozen. A returning name with a
  compatible config reuses its track id; a name not in the moov is skipped
  with a warning naming it, or refused if it would leave the file with no
  track. No re-init mid-stream. A trailing-moov file mode is a separate
  feature.
- `--no-audio`/`--no-video` from [cli-no-role](/quest/m1/cli-no-role.md) is
  how a caller avoids a role entirely.

Tests: an export whose audio starts 2 s before the first video keyframe keeps
that audio; a rendition removed and re-added after the init writes fragments
under its original track id, and the output passes an ffprobe-equivalent
parse.

## Closes

- [#4769](https://github.com/moq-dev/moq/issues/4769) - close this issue when the quest finishes
- [#4770](https://github.com/moq-dev/moq/issues/4770) - close this issue when the quest finishes

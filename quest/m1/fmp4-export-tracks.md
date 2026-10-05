# [M] fMP4 export keeps every track it promised in moov

## Goal

A live `moq export fmp4` recording is always readable and starts with all of
its media. Audio published before the first video keyframe is written, and a
rendition that leaves and returns after the init writes under the track the
moov declared, or the export fails loudly.

## Plan

An fMP4 file declares every track in one moov before any fragment. In
`rs/moq-mux/src/container/fmp4/export.rs` the init waits for each video
track's avcC, which an Annex-B H.264 or H.265 source only provides at its
first keyframe; meanwhile a ready audio track parks on one pending frame and
falls a full max-age budget behind, so its groups are skipped. After the
init, `update_catalog` gives a returning name a fresh `max(id) + 1` track id
that is not in the moov, and ffprobe rejects the file.

Decided (2026-10-04):

- Init from the catalog: write avc3 and hev1 sample entries from the
  catalog's profile and level, with SPS and PPS in-band, so the init is ready
  at the first catalog and the wait disappears for H.264 and H.265. Apple
  prefers hvc1 for HEVC and some editors handle avc3 poorly; that is the
  accepted trade.
- For a codec whose entry still needs the bitstream, drain every ready track
  into a per-track fragment queue before the init, keeping fragment
  boundaries, and flush after the moov. The queue is bounded; exceeding it
  (for example a source that never sends a keyframe) fails the export.
- Tracks are identified by rendition name. Once the moov is out the set is
  frozen. "Compatible" compares the synthesized sample entry, so bitrate and
  framerate churn in the catalog is ignored; the original timescale is kept
  and samples are rescaled. A compatible returning name reuses its track id.
  A name not in the moov, an incompatible config, or a config change while
  present ends the export with an error naming it. A replayed history whose
  decode time goes backwards also ends the export. No re-init mid-stream.
- A recording that must include renditions joining later uses the
  [MP4 export](/quest/m2/mp4-export.md).

Tests: an H.264 Annex-B export whose audio starts 2 s before the first
keyframe keeps that audio; a rendition removed and re-added after the init
writes under its original id and the output parses; a new name, an
incompatible config, and a backwards replay each fail the export.

## Closes

- [#4769](https://github.com/moq-dev/moq/issues/4769) - close this issue when the quest finishes
- [#4770](https://github.com/moq-dev/moq/issues/4770) - close this issue when the quest finishes

## Related

- [Leave out a role](/quest/m1/cli-no-role.md) - how a caller avoids a role entirely

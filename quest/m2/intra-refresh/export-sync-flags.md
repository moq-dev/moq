# [S] Exports stop advertising a refresh group start as a sync sample

## Goal

Exporting or serving a video rendition with `warmup` set no longer lies to
downstream players: fMP4 marks a refresh group's first sample as non-sync and
its fragment as not independent, MKV clears the keyframe flag, and HLS omits
`EXT-X-INDEPENDENT-SEGMENTS` and marks parts `INDEPENDENT=NO`. A group that
opens on a true IDR keeps its flags. MPEG-TS is unchanged: the recovery-point
SEI is in the bitstream and the random access indicator is defined for it.
Audio `warmup` is Opus pre-roll and changes no export flag.

## Plan

- `rs/moq-mux/src/container/fmp4/fragmenter.rs` decides `independent` from
  `frame.keyframe`, which is positional; when a video rendition declares `warmup`
  the decision also needs the IDR check the consumer quest adds to the codec
  modules. The fMP4 and MKV exporters set their sample flags from the same
  answer.
- `rs/moq-hls` derives the playlist tags from the fragmenter's `independent`.
- Tests: a synthetic refresh rendition exports with non-sync group starts and
  an IDR group with sync flags, in fMP4 and MKV; the HLS playlist test covers
  the tags.

## Required

- [Catalog warmup](/quest/m2/intra-refresh/catalog-warmup.md) - the field exports read
- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - the IDR check this reuses

# [S] TS AAC program config survives joins and resumes

## Goal

AAC whose layout lives in a program config element (ADTS `channel_config` 0)
plays over MPEG-TS for a receiver that joins mid-stream or after a resume. A
missing PCE silences that one track instead of failing the whole program.

## Plan

Found in review of the audio-codecs line (#4081), 2026-09-30.

- Import: since #4733 a missing PCE no longer ends the import; it is a
  Damaged unit (`unit_error` in `rs/moq-mux/src/container/ts/import.rs`). But
  the AAC track still holds its catalog reservation until a PCE arrives, and
  the catalog is withheld until every reservation drops
  (`rs/moq-mux/src/catalog/producer.rs`), so a late join to an ffmpeg source
  publishes nothing, video included. Decided in the 2026-10-05 audit: a
  PCE-less AAC track releases its reservation, the catalog publishes without
  it, and the track is added when a PCE arrives. Rejected: keeping the
  reservation and withholding the program. ffmpeg's ADTS muxer writes the PCE
  only in the first frame, so a late join of an ffmpeg source plays without
  that AAC track; say so in the docs.
- Export: `mux` takes the PCE on the first frame only
  (`rs/moq-mux/src/container/ts/export.rs`), so `resume()`/`rewind()` and any
  mid-stream receiver never see it. Repeat it, either on every frame or with
  each PAT/PMT, and pick whichever ffmpeg and the import side accept.
- Regression: an export resumed before its first span re-emits the PCE, and an
  import that starts on a PCE-less frame publishes its catalog with the other
  tracks at once, then adds the AAC track once a PCE arrives.

Public API: none. Wire: none; TS output gains repeated PCEs.

# [S] TS AAC program config survives joins and resumes

## Goal

AAC whose layout lives in a program config element (ADTS `channel_config` 0)
plays over MPEG-TS for a receiver that joins mid-stream or after a resume. A
missing PCE silences that one track instead of failing the whole program.

## Plan

Found in review of the audio-codecs line (#4081), 2026-09-30.

- Import: `aac::in_band_config` returns `ProgramConfigMissing` when the first
  frame seen has no leading PCE, and `?` ends the whole TS import, video
  included (`rs/moq-mux/src/container/ts/import.rs`). Instead, keep the
  reservation, drop frames on that track until one leads with a PCE, and log
  once. ffmpeg's ADTS muxer writes the PCE only in the first frame, so a late
  join of an ffmpeg source stays silent; say so in the docs.
- Export: `mux` takes the PCE on the first frame only
  (`rs/moq-mux/src/container/ts/export.rs`), so `resume()`/`rewind()` and any
  mid-stream receiver never see it. Repeat it, either on every frame or with
  each PAT/PMT, and pick whichever ffmpeg and the import side accept.
- Regression: an export resumed before its first span re-emits the PCE, and an
  import that starts on a PCE-less frame publishes the track once a PCE
  arrives, while the other tracks keep flowing.

Public API: none. Wire: none; TS output gains repeated PCEs.

## Related

- [TS Opus channel codes](/quest/m1/ts-opus-channel-codes.md) - the same per-track refusal principle for Opus descriptors

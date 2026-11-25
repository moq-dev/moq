# [S] A padded TS export carries any audio at its recorded rate

## Goal

`moq export ts` of a source that records a multiplex rate never fails with "MPEG-TS output missed a
decode deadline" for its audio, whatever the codec, and whether or not its catalog declares a bitrate.

## Plan

[A TS AAC source exports with default settings](https://github.com/moq-dev/moq/pull/9609) raised
a padded rate to what the catalog's AAC bitrates need per slot (`audio_floor` in
`rs/moq-mux/src/container/ts/export.rs`). Two gaps remain, both reproducible with the paced export
test beside `an_ffmpeg_aac_source_exports_with_default_settings`:

- AC-3, E-AC-3, MP2 and Opus in TS get no floor, and they pack and lead with tables the same way.
- A catalog entry without a bitrate gets none, and an encoder whose frames peak past twice its
  average still misses a deadline: a pulsed synthetic AAC source had no bitrate and a 3.5x peak.

Recommendation: size the floor from the decoder buffer's own bound per codec instead of the
catalog's average, or find the frame size at the first tables, which is when the rate is last free
to change without a revision.

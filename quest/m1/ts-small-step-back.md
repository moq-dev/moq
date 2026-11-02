# [S] A step back smaller than the delay keeps the TS export

## Goal

An unmarked timestamp step back smaller than the export's delay no longer
fails the MPEG-TS export. Audio restarts its clock instead of regressing within
one clock period. Declared-reorder video stays within its declared depth.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Found by [moq-dev/moq#7221](https://github.com/moq-dev/moq/pull/7221), which
fixed steps larger than the delay. With a −300 ms step at lag 0, audio
regresses within one clock period, and declared-reorder video fails with
"video reorders past its declared depth 2".

- The threshold differs by track type: any regression for audio, and the
  declared depth for video.
- #7221's `Track::steps_back` (`rs/moq-mux/src/container/ts/export.rs`) is
  where it goes.
- Check the narrow case #7221 also raised: a slow, deep B-frame stream that
  declares no reorder depth must not be mistaken for a step.
- Extend `an_unmarked_step_keeps_the_export_rendering`.

## Related

- [TS reliability](/quest/m1/ts-reliability/README.md) - the epic whose step-back fix this extends

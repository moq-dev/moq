# [XS] An operator can see what a TS import carries

## Goal

An operator feeding `import ts` from a source that is already a transport
stream pipes its bytes rather than re-muxing it, and can check what the
import kept. The first PMT logs one info line per elementary stream with its
PID, stream type, and what it became: a track, catalog SI, verbatim, or
dropped.

## Plan

Decided 2026-10-19 by the maintainer (#5950):

- `doc/bin/cli.md`'s `import ts` section adds one sentence: `ffmpeg -c copy
  -f mpegts` re-muxes and drops PIDs (NIT, TDT/TOT, extra audio, teletext,
  SCTE-35) from a TS source, so pipe the bytes instead, for example
  `tsp -I ip … -O file - | moq … import ts`, or use `import srt`. Check the
  example against `--help`.
- The log lives in `handle_pmt` (`rs/moq-mux/src/container/ts/import.rs`),
  once per new elementary stream, beside the existing warn on a dropped
  stream type. It lands after [whole-PES import](/quest/m1/ts-reliability/pes-import.md),
  which rewrites the same `ensure_stream` arms.

Public API: none. Log output only.

## Required

- [Whole-PES import](/quest/m1/ts-reliability/pes-import.md) - rewrites the stream outcomes this log reports

## Closes

- [#5950](https://github.com/moq-dev/moq/issues/5950) - piping a broadcast TS through ffmpeg -c copy drops most of its PIDs

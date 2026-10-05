# [M] moq export mp4 records a crash-safe regular MP4

## Goal

`moq export mp4 --output <file>` records the way OBS's hybrid MP4 does: the
file is a valid fragmented MP4 while recording, so a crash still leaves a
playable file, and a clean end or Ctrl+C finishes it as a regular MP4 with
its moov at the end, without copying media. Renditions that join mid-recording
are included in the finished file.

## Plan

Decided (2026-10-04):

- Layout, as OBS does: `ftyp`, a reserved `free` box, the fragmented moov,
  then moof and mdat pairs. On finish, write a full moov at the end and
  overwrite the `free` header with an mdat header covering everything before
  it. While recording, the reserved box must be `free`: an `mdat` header
  there would swallow the moov and fragments, and a crashed file would be
  unreadable. Moov-first (faststart) would cost a full copy and is not
  planned.
- Renditions that join after the fragmented moov are written into `free`
  boxes, which crash-time readers skip, and indexed only by the final moov.
- The final sample tables come from parsing the written moofs back, which
  needs no API change and also enables a repair command for crashed files.
- A new `mp4` format that requires `--output` (it must seek); `export fmp4`
  stays a stream to stdout.

- Finishing is required, not best effort: the first SIGINT or SIGTERM
  finalizes the file, which needs a graceful-finish path in `moq-cli` (today
  SIGINT drops the task set). A second signal aborts at once and leaves the
  crash-safe fragmented file.
- No size limit: always write co64 chunk offsets and a 64-bit (largesize) mdat
  header.

Tests: SIGINT during a recording produces a regular MP4 that parses with its
moov at the end; a second SIGINT leaves a playable fragmented file.

Open for the PR: edit lists or stretched durations for gaps and track start
offsets; whether a returning rendition with a new config gets an extra sample
description.

## Related

- [fMP4 export tracks](/quest/m1/fmp4-export-tracks.md) - the streaming export, which refuses renditions that join after its moov

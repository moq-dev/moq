# [M] HLS recorder in the CLI

## Goal

`moq ... export hls --output <dir>` writes the broadcast's playlists and
segments to a directory as they complete, using moq-hls's recorder cursors,
and exits with an error if the recording ends abruptly or misses a segment.
It works alone or alongside `--listen`, so one stage can serve and record at
once.

## Plan

Decided 2026-10-23 (maintainer interview):

- A flag beside `--listen`, not a positional target or an object-store URL.
- Recording alone binds no port: `--listen` defaults to `[::]:8089` today, so
  that default must not apply when `--output` is given without `--listen`.
- This is the cursors' first consumer outside moq-hls's tests.
- Update `doc/bin/cli.md` and `doc/bin/hls.md`, and every example that uses
  `export hls`.

Decoder reconfiguration closes the old rendition cursor and adds a replacement.
The replacement cursor starts at the oldest row still in its window, so it can
repeat segment numbers already written by the old cursor. Settle recorded
playback before implementing the output layout:

- Separate instance directories preserve both cursors independently, but a master
  listing only current instances cannot play the recording's earlier part after
  reconfiguration.
- A continuous playlist per logical rendition must drain the old cursor, discard
  repeated rows from the replacement, write its new init, and mark the decoder
  transition without losing or duplicating media. Coordinate discontinuities
  across the recording's audio and video playlists.

Prefer continuity for a full recording. Confirm that contract and the overlap
policy before choosing the layout; the CLI flags above are already decided.

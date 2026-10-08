# [M] History mode lists a recording from its start

## Goal

An HLS export in history mode lists a recording from its first segment,
however long it is. Today it starts from what the timeline restates when the
exporter joins: at most 256 records (`CHECKPOINT_RECORDS` in `moq-mux`'s
`timeline.rs`), so a recording longer than about 8.5 minutes of 2 s segments
lists from its middle.

## Plan

Found by #4978 (bounded HLS playlists), which kept the full listing behind
`moq_hls::export::Config::history` and confirmed the gap with a 4-record limit:
12 replayed segments listed from `EXT-X-MEDIA-SEQUENCE:8`. The archive test
missed it because its hand-built timeline repeats every record.

Decided 2026-10-07: read the stored timeline groups in history mode rather
than only the restated tail, so the exporter or the archive reader replays
the history it holds. Pick where it lives (the exporter fetching past timeline
groups, or `moq_archive::Reader` restating the whole stored timeline) by what
keeps the live path bounded: live joins must keep reading at most the window.

Test with more records than one checkpoint and assert history mode lists
segment 0.

Public API: none expected. Wire: none.

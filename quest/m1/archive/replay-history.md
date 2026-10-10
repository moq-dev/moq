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

Draft implementation reads a configured subscriber from group zero with the full
representable delay budget, keeping old timed groups eligible. A bounded four-record
fixture now lists all 12 segments from zero; ordinary live joins stay bounded.
The proposed `timeline::Consumer::new(subscriber, timescale)` constructor lets the
exporter configure that subscription without coupling history policy to the decoder.

Remaining limitation: `Reader` replays timeline groups into the ordinary broadcast
cache and its FETCH handler serves media groups, not timeline groups. Once old
timeline groups are evicted, starting at zero cannot recover them. The draft fixes
checkpoint-tail joins but does not yet satisfy arbitrary-length cached-out history.

Recommended next step: support durable timeline FETCH replay. Alternatives are a
dedicated unbounded timeline cache or Reader full-history restatement; both change
memory or live-join costs. Keep this quest open until the durable path is settled.

Public API: proposed configured-subscriber Consumer constructor. Wire: none.

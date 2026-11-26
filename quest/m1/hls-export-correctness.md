# [M] Fix moq-hls Hold metering leak, grid timing, NTSC gaps, GROUP_END closure, bounded fetches, and playlist/import conformance

## Goal

`moq-hls` stops metering unrequested bytes in `Hold`, emits accurate DASH
`S@t`/`S@d` and HLS `EXT-X-PROGRAM-DATE-TIME` after pauses and overruns, stops
emitting spurious millisecond `EXT-X-GAP` segments on 29.97fps NTSC GOP jitter,
closes segments immediately on lite-07 `GROUP_END`, fetches multi-group
segments in one bounded `SUBSCRIBE`, emits valid master playlist versions and
`EXT-X-INDEPENDENT-SEGMENTS`, and imports non-H.264 (HEVC, AV1, VP9) HLS master
playlists.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS audit):

- ✅ Keep the time grid for deterministic cross-edge segment IDs and live
  `EXT-X-TARGETDURATION = 2 * segment_length`, fixing NTSC/slight-overrun gap
  spam and `Row.pts` (rejected: replacing the grid with a wire group sequence;
  edge-local `EXT-X-MEDIA-SEQUENCE` with sticky routing; tightening live
  `EXT-X-TARGETDURATION`; leaving grid gap logic as-is).

Fixes in `rs/moq-hls`:

1. **`Hold` metering leak (`src/export/renditions.rs`):** call `.unmetered()`
   before subscribing in `Hold` (matching `WatchMedia`), so metered origins
   never count media bytes that no HTTP client requested before `sync.interval`
   is declared.
2. **`Row.pts` actual media PTS (`src/export/rendition.rs`):** preserve
   `grid::Slot::Media`'s actual first-group `pts` alongside grid `start`
   (the `EXTINF` anchor). Use actual media `pts` (and actual media duration)
   for DASH `S@t`/`S@d` (matching CMAF `tfdt`) and for HLS
   `EXT-X-PROGRAM-DATE-TIME` and `segments::Segment.program_date_time`.
3. **NTSC / slight-overrun gap spam (`src/export/grid.rs`):** absorb sub-frame
   or NTSC GOP jitter (e.g. 29.97fps 60-frame GOP = 2,002 ms on a 2,000 ms
   grid) without inserting spurious millisecond `#EXT-X-GAP` segments or
   bumping the playlist to `#EXT-X-VERSION:8`. Keep live
   `EXT-X-TARGETDURATION = 2 * segment_length`.
4. **Immediate segment closure on `GROUP_END` (`src/export/rendition.rs`):**
   read lite-07 `group.end()` in `WatchMedia` so a finished group settles its
   segment immediately (before a pause or at track end) without waiting for a
   subsequent group or decoding trailing frames.
5. **Bounded `SUBSCRIBE` for multi-group segments (`src/export/rendition.rs`):**
   replace `Fetching::Groups`'s serial per-group `track.fetch_group()` loop
   with one bounded `SUBSCRIBE` range `[first..=last]` (eliminating N serial
   round trips for multi-group audio segments on cold caches or archives).
6. **Master/media playlist tags (`src/export/master.rs`, `src/export/playlist.rs`):**
   lower `master.rs`'s hardcoded `#EXT-X-VERSION:9` to match the tags actually
   emitted, and emit `#EXT-X-INDEPENDENT-SEGMENTS` when non-warmup segments
   open on keyframes.
7. **Multi-codec HLS import (`src/import.rs`):** extend `select_variants`
   beyond H.264 (`avc1.`/`avc3.`) to recognize HEVC (`hvc1.`/`hev1.`), AV1
   (`av01.`), and VP9 (`vp09.`) master playlists, matching `moq-mux` fMP4
   import support.

Public API: none. Wire: none. Add unit/integration tests for each fix,
including a 29.97fps NTSC stream, a pause/overrun DASH `S@t` vs `tfdt` check,
a metered `Hold` check, and a non-H.264 master playlist import.

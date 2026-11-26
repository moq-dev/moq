# [M] Fix moq-hls Hold metering leak, grid timing, NTSC gaps, GROUP_END extents, bounded fetches, and playlist/import conformance

## Goal

`moq-hls` stops metering unrequested bytes in `Hold`, emits accurate DASH
`S@t`/`S@d` (with matching `$Time$` segment lookup) and HLS
`EXT-X-PROGRAM-DATE-TIME` after pauses and overruns, stops emitting spurious
millisecond `EXT-X-GAP` segments on 29.97fps NTSC GOP jitter, settles group
extents on lite-07 `GROUP_END`, fetches multi-group segments in one bounded
`SUBSCRIBE`, emits valid master playlist versions and
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
2. **`Row` first-sample PTS and DASH `$Time$` lookup (`src/export/segments.rs`, `src/export/rendition.rs`):**
   `Row` lives in `src/export/segments.rs:143` (with wall-clock callers at
   `segments.rs:614, 672` and `rendition.rs:1302`). When a timed group starts
   before its first frame, the grid stores the group start while CMAF `tfdt`
   uses the first muxed sample's PTS. Carry the first muxed sample's PTS
   separately on `Row` for DASH `S@t`/`S@d` and HLS
   `EXT-X-PROGRAM-DATE-TIME` / `segments::Segment.program_date_time`, keeping
   grid placement for segment numbering and `EXTINF`. Update DASH `$Time$`
   lookup (`segment_number_at` at `src/export/segments.rs:390-400`) to match
   the exact same converted first-sample PTS that `S@t` renders, so
   `seg/{tag}.t{time}.m4s` URLs advertised by the MPD never 404.
3. **NTSC / slight-overrun gap spam (`src/export/grid.rs`):** absorb sub-frame
   or NTSC GOP jitter (e.g. 29.97fps 60-frame GOP = 2,002 ms on a 2,000 ms
   grid) without inserting spurious millisecond `#EXT-X-GAP` segments or
   bumping the playlist to `#EXT-X-VERSION:8`. Keep live
   `EXT-X-TARGETDURATION = 2 * segment_length`.
4. **Settle group extent on `GROUP_END` (`src/export/rendition.rs`):**
   read lite-07 `group.end()` in `WatchMedia` to settle each finished group's
   extent without waiting for a subsequent group or decoding trailing frames.
   Publish a segment only once that extent or another boundary proves the
   entire grid slot is complete, so multi-group segments (such as four audio
   groups packed into one HLS segment) are never published incomplete after
   the first group's `GROUP_END` or mutated at an already-published URL.
5. **Bounded `SUBSCRIBE` for multi-group segments (`src/export/rendition.rs`):**
   replace `Fetching::Groups`'s serial per-group `track.fetch_group()` loop
   with one bounded `SUBSCRIBE` range `[first..=last]` (eliminating N serial
   round trips for multi-group audio segments on cold caches or archives).
6. **Master/media playlist tags (`src/export/master.rs`, `src/export/playlist.rs`):**
   lower `master.rs`'s hardcoded `#EXT-X-VERSION:9` to match the tags actually
   emitted, and emit `#EXT-X-INDEPENDENT-SEGMENTS` when every listed segment
   opens on a group and every group opens on a keyframe.
7. **Multi-codec HLS import (`src/import.rs`):** extend `select_variants`
   beyond H.264 (`avc1.`/`avc3.`) to recognize HEVC (`hvc1.`/`hev1.`), AV1
   (`av01.`), and VP9 (`vp09.`) master playlists, matching `moq-mux` fMP4
   import support.

Public API: none. Wire: none. Add unit/integration tests for each fix,
including a 29.97fps NTSC stream, a pause/overrun DASH `S@t` vs `tfdt` check
that fetches the exact `$Time$` segment URL advertised by the MPD, a metered
`Hold` check, and a non-H.264 master playlist import.

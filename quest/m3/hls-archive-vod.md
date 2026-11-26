# [M] Zero-payload archive and VOD HLS indexing in moq-hls

## Goal

`moq-archive` answers metadata-only subscriptions from a lightweight
group-timestamp index without downloading media payload objects from object
storage, and `moq-hls` renders VOD (`#EXT-X-PLAYLIST-TYPE:VOD` +
`#EXT-X-ENDLIST` on finished tracks) and `[start, end)` range playlists by
lazily indexing only the requested rendition over a metadata-only subscription,
with `EXT-X-TARGETDURATION` computed as `ceil(max(EXTINF))` of the actual
playlist instead of `2 * segment_length`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS audit):

- ✅ Gate archive/VOD indexing in `moq-hls` on metadata-only subscriptions
  ([subscribe-meta.md](/quest/m2/subscribe-meta.md)) plus a lightweight archive
  group index so playlist rendering never downloads media payloads (rejected:
  lazy per-rendition full-object scan before metadata-only; synthetic
  fixed-grid VOD playlist without scanning groups; deferring archive/VOD
  planning).
- ✅ Compute VOD `EXT-X-TARGETDURATION` from the scanned metadata as
  `ceil(max(EXTINF))` (keeping `2 * segment_length` only for live sliding
  playlists where future GOP phase shifts are unknown).

Scope:

1. **`moq-archive` group-timestamp index (`rs/moq-archive`):**
   - Persist lightweight group start/end metadata (and frame header timestamps
     if needed for grid placement) alongside archived tracks so `moq-archive`'s
     consumer answers metadata-only subscriptions without fetching media
     payload objects from object storage.
2. **`moq-hls` VOD and range playlist mode (`rs/moq-hls`):**
   - Restore VOD/history and `[start, end)` range playlist rendering on
     `Broadcaster` / `Rendition` / `Server` (emitting `#EXT-X-PLAYLIST-TYPE:VOD`
     and `#EXT-X-ENDLIST` once a track is finished, and setting
     `EXT-X-TARGETDURATION` to `ceil(max(EXTINF))` from the scanned metadata).
   - Index archive renditions lazily per requested rendition over a
     metadata-only subscription (`Start::Oldest` or `Anchor::Time(start)`), so
     `master.m3u8` performs zero media-object GETs and `media.m3u8` reads only
     the requested rendition's metadata index; media payload objects are
     fetched only when an HTTP client requests a segment.
3. **Storage-traffic tests (`rs/moq-hls/src/export/archive_tests.rs`):**
   - Tighten `archive_tests.rs` on multi-rendition recordings to assert that
     `master.m3u8` and `media.m3u8` perform zero media-object GETs across all
     renditions, and that a segment request downloads only objects covering
     that segment's groups.

Public API: additive VOD/range playlist API in `moq-hls` and additive archive
index format in `moq-archive` (backward-compatible fallback for archives
without the index). Wire: none beyond
[subscribe-meta.md](/quest/m2/subscribe-meta.md).

## Required

- [Metadata-only subscriptions](/quest/m2/subscribe-meta.md) - metadata-only subscriber API so archive playlist rendering downloads zero media payloads

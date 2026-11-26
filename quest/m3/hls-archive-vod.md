# [M] Zero-payload archive and VOD HLS indexing in moq-hls

## Goal

`moq-archive` stays container-agnostic and answers metadata-only subscriptions
via ranged reads of its existing per-object frame tables without downloading
media payload bytes from object storage; and `moq-hls` renders VOD
(`#EXT-X-PLAYLIST-TYPE:VOD` + `#EXT-X-ENDLIST` on finished tracks) and
`[start, end)` range playlists by lazily indexing only the requested rendition
over a metadata-only subscription (with targeted payload probing only on
inline-parameter-set video renditions that can resize without a catalog
update), computing `EXT-X-TARGETDURATION` as `ceil(max(EXTINF))` of the actual
playlist instead of `2 * segment_length`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS audit):

- ✅ Gate archive/VOD indexing in `moq-hls` on metadata-only subscriptions
  ([subscribe-meta.md](/quest/m2/subscribe-meta.md)) backed by header-only
  archive reads so playlist rendering never downloads media payloads for
  catalog-described renditions (rejected: lazy per-rendition full-object scan
  before metadata-only; synthetic fixed-grid VOD playlist without scanning
  groups; deferring archive/VOD planning).
- ✅ Compute VOD `EXT-X-TARGETDURATION` from the scanned metadata as
  `ceil(max(EXTINF))` (keeping `2 * segment_length` only for live sliding
  playlists where future GOP phase shifts are unknown).

Scope:

1. **Container-agnostic metadata serving in `moq-archive` (`rs/moq-archive`):**
   - Keep `moq-archive` container-agnostic (depending only on `moq-net`, never
     `moq-mux` or `hang`).
   - Extend `moq-archive`'s container-agnostic per-object frame-table header
     (archives break in place per `quest/m2/README.md`, so add no dual-format
     fallback) to persist each group's explicit `GROUP_END` extent and the
     publisher's declared skip ranges (`SUBSCRIBE_DROP(SKIP)`) alongside the
     group starts, frame timestamps, and frame lengths it already writes.
   - Because archive objects are write-once (`rs/moq-archive/src/producer/track.rs:365`),
     when a `GROUP_END` extent or declared skip arrives after the flush timer
     has already written a group's last media frame (such as before a pause
     with no subsequent frames), write a small write-once metadata-only archive
     record carrying that extent/skip so replay always finds the pause
     boundary even if no more frames arrive.
   - Answer metadata-only subscriptions via ranged reads of those frame-table
     headers and metadata-only records, emitting the persisted frame headers,
     `GROUP_END` extents, and declared skips without downloading media payload
     bytes from object storage.
2. **`moq-hls` VOD and range playlist mode (`rs/moq-hls`):**
   - Restore VOD/history and `[start, end)` range playlist rendering on
     `Broadcaster` / `Rendition` / `Server` (emitting `#EXT-X-PLAYLIST-TYPE:VOD`
     and `#EXT-X-ENDLIST` once a track is finished, and setting
     `EXT-X-TARGETDURATION` to `ceil(max(EXTINF))` from the scanned metadata).
   - Index archive renditions lazily per requested rendition over a
     metadata-only subscription (`Start::Oldest` or `Anchor::Time(start)`).
     Pauses and group extents come from wire `GROUP_END` and
     `SUBSCRIBE_DROP(SKIP)` plus tiny-size candidate marker probing when
     ambiguous (matching `hls-server-lifecycle.md`), and codec changes come
     from the archived catalog; retain targeted keyframe probing in `moq-hls`
     only for inline-parameter-set video renditions that can resize without a
     catalog update. Media payload bytes for catalog-described renditions are
     fetched only when an HTTP client requests a segment.
3. **Storage-traffic tests (`rs/moq-hls/src/export/archive_tests.rs`):**
   - Tighten `archive_tests.rs` on multi-rendition catalog-described recordings
     to assert that `master.m3u8` performs zero track object reads,
     `media.m3u8` performs only ranged frame-table reads (zero media payload
     bytes downloaded) for the requested rendition alone, and a segment request
     downloads only payload bytes covering that segment's groups.

Public API: additive VOD/range playlist API in `moq-hls`. Wire: none beyond
[subscribe-meta.md](/quest/m2/subscribe-meta.md).

## Required

- [Metadata-only subscriptions](/quest/m2/subscribe-meta.md) - metadata-only subscriber API so archive playlist rendering downloads zero media payloads

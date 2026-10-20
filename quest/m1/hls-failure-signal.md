# [S] A failed HLS export answers with an error

## Goal

Once an exporter's timeline fails, in live or history mode, its media
playlists and the DASH manifest answer a 5xx with `Cache-Control: no-store`
instead of ending with `EXT-X-ENDLIST`, so a truncated recording never looks
complete and no CDN caches a short one. `EXT-X-ENDLIST` and the DASH static
manifest stay reserved for a clean finish. A rendition whose own timeline
fails errors only its own playlist, so players can switch to a sibling.

## Plan

Decided by the maintainer on 2026-10-19, over clearing a history playlist to
an ended empty one (still reads as finished) and an in-band marker (HLS and
DASH have none; `EXT-X-GAP` needs durations a failed timeline cannot give).

Today `watch_timeline` (`rs/moq-hls/src/export/mod.rs`) ends every playlist
through `renditions::Fanout::finish` on success and failure alike. Give the
feed a failed state beside `ended`, set it on a timeline error, and have the
playlist and manifest routes (`rs/moq-hls/src/server/routes.rs`) answer the
existing no-store error response for a failed feed. Late-created renditions
start failed like they start ended. Segments already listed may stay
fetchable; only the listings refuse.

Test a mid-history group that fails to FETCH or decode, a live timeline
error, a single failed rendition beside a healthy one, and the DASH manifest.
Update `doc/` where it describes how an export ends.

Public API: none expected. Wire: none; HTTP responses change for a failed
export.

## Related

- [History from the retained floor](/quest/m1/archive/history-floor.md) - the other history-mode gap left by #5236

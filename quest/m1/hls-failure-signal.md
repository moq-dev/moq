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

Failures take three paths today, and all three end the playlist as finished:

- The shared reference timeline: `watch_timeline`
  (`rs/moq-hls/src/export/mod.rs`) calls `renditions::Fanout::finish` on
  success and failure alike. Give the feed a failed state beside `ended`, set
  on a timeline error; late-created renditions start failed like they start
  ended.
- A retained past reference: in history mode `watch_catalog` keeps old
  reference watchers after a handoff, and when one errors `watch_timeline`
  takes the `finish(...) == false` branch and retires its rows
  (`renditions.pop`), so the recording loses a prefix and still ends as
  complete. Mark the feed failed there too instead of only retiring the rows.
- A rendition's own timeline: `watch_spans` calls `Spans::fail`
  (`rs/moq-hls/src/export/spans.rs`), and `Rendition::snapshot_as`
  (`rendition.rs`) folds a failed last row into `finished`. Expose that
  rendition-local failure to the playlist route instead of rendering
  `EXT-X-ENDLIST`. `Rendition::playlist` waits in `playable()` (and then
  `init()`), which watch only the live window, so the failure must also wake
  those waits: otherwise a request pending before the first row times out as a
  404, and a router-free caller waits forever.

The media playlist routes (`rs/moq-hls/src/server/routes.rs`) answer the
existing no-store error response for any of these failures. The DASH manifest
is broadcast-wide, so it answers that error only for a shared or retained
reference failure; a rendition-local failure omits that representation and
keeps its siblings available. Segments already listed may stay fetchable;
only the listings refuse.

Test a mid-history group that fails to FETCH or decode, a ranged clip over a
failed timeline (5xx, not 416), a playlist request already pending when the
timeline fails before its first row, a retained reference
failing after a handoff, a live timeline error, a single failed rendition
beside a healthy one (its playlist errors, the sibling's and the DASH
manifest's do not), and the DASH manifest after a shared failure.
Update `doc/` where it describes how an export ends.

Embedders that skip the server router read the same failure from the public
API: every public accessor that renders a listing reports it as an error, so a
failed timeline differs from a listing that is not available yet. Today those
are `Broadcaster::manifest` and `Rendition::media_playlist` (both
`Option<String>`, becoming `Result<Option<String>>`) and `Rendition::playlist`
(already a `Result`). `Broadcaster::manifest` errors only for a shared or
retained reference failure; a failed rendition drops from the manifest while its
own `Rendition` accessors error. The routes map those errors to the 5xx. This
includes the ranged history
path: `snapshot_range` turns a failed timeline into `Error::RangeUnavailable`,
which answers 416 as if the client asked for a bad range; it returns the
failed-timeline error instead. Audit every snapshot path for the same fold
rather than relying on this list.

Public API: breaking, `export::Broadcaster::manifest` and
`export::Rendition::media_playlist` return a `Result`, and a new `Error`
variant for a failed timeline; add a `MIGRATE.md` entry. Wire: none; HTTP
responses change for a failed export.

## Related

- [History from the retained floor](/quest/m1/archive/history-floor.md) - the other history-mode gap left by #5236

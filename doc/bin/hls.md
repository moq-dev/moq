---
title: HLS
description: Serve a broadcast as HLS or DASH, or import an HLS playlist
---

# HLS

`moq export hls` serves a MoQ broadcast as HLS and DASH over HTTP for players
that can't speak MoQ. `moq import hls` pulls a remote HLS master or media
playlist into a broadcast.

```bash
# Serve. Players open http://localhost:8089/my-stream.hang/master.m3u8 (or manifest.mpd for DASH)
moq --connect https://relay.example.com/anon --broadcast my-stream.hang export hls --listen '[::]:8089'

# Import
moq --connect https://relay.example.com/anon --broadcast my-stream.hang import hls https://example.com/live/master.m3u8
```

The CLI serves the one `--broadcast` it was given, at
`/{broadcast}/master.m3u8` and `/{broadcast}/manifest.mpd`.

Import follows a live playlist, joining a few segments behind its live edge,
until every rendition's playlist ends with `EXT-X-ENDLIST`. It then keeps the
finished broadcast up until its media is stale
(see [retention](/bin/cli#retention-and-latency)). A playlist that has already
ended, such as a VOD recording, is refused with an error rather than imported.
An `EXT-X-DISCONTINUITY` keeps the source's timestamps. Timestamp resets are not
supported: if a rendition's timestamps go backwards under the same `EXT-X-MAP`,
such as an encoder restart, the import fails with an error. Import the restarted
source as a new broadcast.

## How export works

Export never subscribes to media. It reads each rendition's
[timeline track](/concept/hang#catalog), a small index of that rendition's
groups, to build playlists. A segment request fetches exactly the frames it
covers from the relay's cache and transmuxes them to CMAF. Media is never
transcoded, so the player must support the publisher's codecs.

What that means for you:

- **Idle renditions cost nothing**, and a segment is servable for as long as the relay's [cache](/bin/relay/config#cache) retains it. Keep `--window` (the playlist length, default 16 s) within that retention.
- **Live joins cost the window.** The default exporter reads the timeline checkpoint and lists at most 256 segments. `export::Config::history` reads a durable recording's retained timeline groups from the beginning instead of joining only the checkpoint tail.
- **Publish timelines.** A broadcast whose catalog advertises no timelines is skipped, and a rendition is listed only once its own timeline is advertised. Without a duration hint, a publisher declares that timeline after the rendition's first segment.
- **Align GOPs across video renditions.** Segment boundaries come from one reference rendition. Other video renditions snap to their nearest keyframe, and a segment with none nearby becomes an `EXT-X-GAP`.
- **Disabled renditions are hidden.** A rendition the catalog marks `enabled: false` leaves the multivariant playlist and DASH manifest until it re-enables. Its media playlist keeps serving, so a player already on it is not ended with `EXT-X-ENDLIST`. If it supplied segment boundaries, an enabled sibling takes over at its first record numbered past the last advertised one, and DASH starts a new period. The media sequence is the record number, so every edge agrees: a sibling numbered ahead restarts the playlist with a discontinuity, and one numbered behind (such as a re-enabled rung) holds every playlist until its numbering catches up. `EXT-X-DISCONTINUITY-SEQUENCE` and DASH periods are counted per edge, so edges agree on them only when no discontinuity or handoff happened while each was running.
- **Segment URLs are stable.** Every edge and every reload derive the same URLs, so a CDN in front of export caches them.
- **Only a clean finish ends a playlist.** `EXT-X-ENDLIST` and a `static` DASH manifest mean the timeline finished. If the segment-boundary rendition's timeline fails instead, such as a publisher dropping its session, every media playlist and the DASH manifest answer HTTP 500 with `Cache-Control: no-store`, so a truncated recording never looks complete. A rendition whose own timeline fails answers the error alone and leaves the multivariant playlist and DASH manifest, so players switch to a sibling.

A [`moq-archive`](https://docs.rs/moq-archive) recording replayed through its
`Reader` is served the same way, with no second stored copy, and lists the
same window as a live broadcast by default. `export::Config::history` includes
earlier timeline groups even after replay-cache eviction: the exporter FETCHes
missing groups from the Reader's stored timeline objects before following live
edits. Missing or unreadable stored history fails that export with an error,
and so does a recording whose start was pruned by retention.

With `export::Config::history` enabled, a durable recording's media playlists
also accept `start` and `end` query parameters in decimal media-time seconds.
Both are required and name a half-open range, for example
`media.m3u8?start=12.5&end=30`. The playlist includes every overlapping
segment, widening its start to the preceding video sync point, and adds
`EXT-X-ENDLIST` once media covers the requested end. It remains an event
playlist while that end is still ahead of the live edge. A malformed range
returns HTTP 400. A live-only timeline or range past a finished recording
returns HTTP 416 instead of a partial clip, and a failed timeline returns 500.

`--listen-tls-cert`/`--listen-tls-key` or `--listen-tls-generate` serve HTTPS,
and `--cors-origin` opens it to browsers. See `moq export hls --help`.

## Import

Import handles classic HLS; LL-HLS parts are not implemented. It publishes the
playlist's own media times, and anchors the catalog clock on the first imported
segment. Each fMP4 segment is split into one frame per sample; `--passthrough`
publishes each fragment whole as CMAF instead, keeping its exact sample timing.

The library, including the recorder cursors for mirroring a broadcast to
storage, is [`moq-hls`](https://docs.rs/moq-hls).

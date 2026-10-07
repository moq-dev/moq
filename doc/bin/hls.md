---
title: HLS
description: Serve any broadcast as HLS, or import an HLS playlist
---

# HLS

`moq export hls` serves a MoQ broadcast as HLS over HTTP for players that
can't speak MoQ. `moq import hls` pulls a remote HLS master or media playlist
into a broadcast.

```bash
# Serve. Players open http://localhost:8089/my-stream.hang/master.m3u8
moq --connect https://relay.example.com/anon --broadcast my-stream.hang export hls --listen '[::]:8089'

# Import
moq --connect https://relay.example.com/anon --broadcast my-stream.hang import hls https://example.com/live/master.m3u8
```

Export never subscribes to media. It reads each rendition's
[timeline track](/concept/hang#catalog), a small index of that rendition's
groups, to build playlists, then fetches exactly the frames a requested segment
covers from the relay's cache and transmuxes them to CMAF on demand. So a
segment is servable for as long as the relay's
[cache](/bin/relay/config#cache) retains it, and idle renditions cost nothing.
Segment boundaries come from one reference rendition (the first video
rendition by name, or the first audio one without video) and are numbered by
its records, so every edge and every reload agree. Every other video rendition
snaps each boundary to its nearest keyframe within about a second, and a
segment with none in range renders as `EXT-X-GAP`; audio takes every frame
inside the segment's span. A jump in content time renders as
`EXT-X-DISCONTINUITY`. Gaps are a fallback: a publisher wanting clean HLS
export should align video GOPs across renditions. A timeline that fails
(malformed, refused, or lost) is not retried, since the relay already rides out
transient source failures. The reference timeline failing ends every playlist
with `EXT-X-ENDLIST`, and any other rendition's timeline failing ends that
rendition's playlist at the last segment it covers. A broadcast
whose catalog advertises no timelines is skipped. One server exposes every
broadcast by path:

```text
/{broadcast}/master.m3u8
/{broadcast}/manifest.mpd
/{broadcast}/{video|audio}/{rendition}/media.m3u8
/{broadcast}/{video|audio}/{rendition}/init.{hash}.mp4
/{broadcast}/{video|audio}/{rendition}/seg/{reference}.{segment}.m4s
/{broadcast}/{video|audio}/{rendition}/seg/{reference}.t{pts}.m4s
```

Segment boundaries come from one reference rendition's timeline, and its records
number the segments. `{reference}` is a short hash of that rendition's kind and
name, so every edge derives the same URL, and a reference that changes (a new
first video rendition) starts a new numbering under new URLs rather than reusing
the old ones for other content.

A [`moq-archive`](https://docs.rs/moq-archive) recording replayed through its
`Reader` is served the same way, with no second stored copy. Playlists come
from the replayed timelines alone, and a segment GETs only its rendition's
stored objects, so switching renditions never downloads both. An
inline-parameter-set codec with no catalog `description` is the exception:
the first playlist render GETs one keyframe group to build the init segment,
then caches it. Out-of-band configs need no media GET. A recording's playlists
list the same capped window as a live broadcast. A library embedder can set
`export::Config::history` to list past it instead: when the catalog's `archive`
entry names a `store` and no `replay` path, its spans are durable on this
broadcast, so only the recording's own retention trims the playlists and DASH
`timeShiftBufferDepth` is the listed span. The listing starts at the records
the timeline restates when the exporter joins (at most 256 from a `moq-mux`
publisher). The playlist ends with `EXT-X-ENDLIST` only once the reader's caller
declares the recording finished; the store holds no completion marker.

A viewer joining a days-old broadcast costs what the window lists, not the
broadcast's age: the exporter reads only the records its timeline restates on
join, never the whole history. The window lists at most 256
segments however short they are, so every edge shows the same
`EXT-X-MEDIA-SEQUENCE`. A rendition with no media for a span lists that span as
a duration-preserving `EXT-X-GAP` and goes on listing after it. The master
playlist advertises a video rendition only once a listed segment starts at a
group start that is a sync point, so an early master may list audio alone, and
answers 404 while no rendition can start.

The init URL carries a hash of its bytes, so a reconfigured rendition gets a
new one. An embedder of the library can also label the publisher's run with
`Broadcaster::set_generation`. Every segment URL then carries it
(`seg/{generation}.{reference}.{segment}.m4s`), since a restarted publisher reuses segment
numbers for different media.

`--window` sets the playlist duration (default 16 s) and caps segment
`Cache-Control: max-age` for every broadcast,
`--listen-tls-cert`/`--listen-tls-key` or `--listen-tls-generate` serve HTTPS,
and `--cors-origin` opens it to browsers.
H.264/H.265 and AAC/Opus renditions are served. Import handles classic HLS;
LL-HLS parts are not implemented yet. It publishes the playlist's own media
times, and the catalog clock maps the first imported segment to the time it
arrived, one mapping for every rendition. The library is
[`moq-hls`](https://docs.rs/moq-hls).

## Recording segments

`moq_hls::export::segments::Segment::discontinuity` is the absolute timeline
sequence within one `Broadcaster`. Cursors created at different times report
the same sequence for the same timeline span, including after a rendition
rebinds. Skipping unavailable segments does not reset the sequence.

HLS numbers a segment as `EXT-X-DISCONTINUITY-SEQUENCE` plus the
`EXT-X-DISCONTINUITY` tags before it (RFC 8216, section 6.2.1), and matching
content in every rendition must share that number. A recorder writes the first
retained segment's value as `EXT-X-DISCONTINUITY-SEQUENCE`, then writes
`current - previous` `EXT-X-DISCONTINUITY` tags before each later segment. The
difference can exceed one when the cursor skipped every segment of an epoch, and
one tag per change would then fall behind sibling renditions. Retain the
absolute value in the index alongside each segment.

Recreating the broadcaster starts a new sequence namespace. Start a new
recording/playlist, or explicitly map the new broadcaster's sequences into a
recording-wide sequence with a discontinuity at the boundary. Do not compare
raw values across broadcaster instances or infer a restart from zero alone.

When updating moq.pro's moq-hls pin to this breaking release, update its recorder
and index together: store the returned sequence directly instead of accumulating
per-cursor break counts. Existing indexes containing counts need conversion
within their original recording namespace before combining them with new data.

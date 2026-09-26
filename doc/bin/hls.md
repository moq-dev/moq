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
export should align video GOPs across renditions. A broadcast
whose catalog advertises no timelines is skipped. One server exposes every
broadcast by path:

```text
/{broadcast}/master.m3u8
/{broadcast}/manifest.mpd
/{broadcast}/{video|audio}/{rendition}/media.m3u8
/{broadcast}/{video|audio}/{rendition}/init.{hash}.mp4
/{broadcast}/{video|audio}/{rendition}/seg/{segment}.m4s
/{broadcast}/{video|audio}/{rendition}/seg/t{pts}.m4s
```

A [`moq-archive`](https://docs.rs/moq-archive) recording replayed through its
`Reader` is served the same way, with no second stored copy. Playlists come
from the replayed timelines alone, and a segment GETs only its rendition's
stored objects, so switching renditions never downloads both. An
inline-parameter-set codec with no catalog `description` is the exception:
the first playlist render GETs one keyframe group to build the init segment,
then caches it. Out-of-band configs need no media GET. When the catalog's
`archive` entry names a `store` and no `replay` path, its spans are durable on
this broadcast, so the playlists list the whole retained timeline and only the
recording's own retention trims them; DASH `timeShiftBufferDepth` is the listed
span. The playlist ends with `EXT-X-ENDLIST` only once the reader's caller
declares the recording finished; the store holds no completion marker.

The init URL carries a hash of its bytes, so a reconfigured rendition gets a
new one. An embedder of the library can also label the publisher's run with
`Broadcaster::set_generation`. Every segment URL then carries it
(`seg/{generation}.{segment}.m4s`), since a restarted publisher reuses segment
numbers for different media.

`--window` sets the live playlist duration (default 16 s) and caps segment
`Cache-Control: max-age` for every broadcast,
`--listen-tls-cert`/`--listen-tls-key` or `--listen-tls-generate` serve HTTPS,
and `--cors-origin` opens it to browsers.
H.264/H.265 and AAC/Opus renditions are served. Import handles classic HLS;
LL-HLS parts are not implemented yet. The library is
[`moq-hls`](https://docs.rs/moq-hls).

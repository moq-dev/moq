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

Export never subscribes to media. It reads the broadcast's
[timeline track](/concept/hang#catalog), a log of complete segments aligned
across every rendition, to build playlists, then fetches exactly the groups a
requested segment covers from the relay's cache and transmuxes them to CMAF on
demand. So a segment is servable for as long as the relay's
[cache](/bin/relay/config#cache) retains it, and idle renditions cost nothing.
Because segments are aligned, the same number names the same span of content in
every media playlist; a record with nothing for a rendition renders as
`EXT-X-GAP` and a jump in content time as `EXT-X-DISCONTINUITY`. A broadcast
whose catalog advertises no timeline is skipped. One server exposes every
broadcast by path:

```text
/{broadcast}/master.m3u8
/{broadcast}/manifest.mpd
/{broadcast}/{video|audio}/{rendition}/media.m3u8
/{broadcast}/{video|audio}/{rendition}/init.{hash}.mp4
/{broadcast}/{video|audio}/{rendition}/seg/{segment}.m4s
/{broadcast}/{video|audio}/{rendition}/seg/t{pts}.m4s
```

The init URL carries a hash of its bytes, so a reconfigured rendition gets a
new one. An embedder of the library can also label the publisher's run with
`Broadcaster::set_generation`. Every segment URL then carries it
(`seg/{generation}.{segment}.m4s`), since a restarted publisher reuses segment
numbers for different media.

`--window` sets the playlist duration (default 16 s),
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

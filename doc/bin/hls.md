---
title: HLS
description: Serve a broadcast as HLS or DASH, or import an HLS playlist
---

# HLS

`moq export hls` serves a MoQ broadcast as HLS and DASH over HTTP for players
that can't speak MoQ, records it as HLS files, or both. `moq import hls` pulls
a remote HLS master or media playlist into a broadcast.

```bash
# Publish, then serve. Players open
# http://localhost:8089/my-stream.hang/master.m3u8 (or manifest.mpd for DASH)
ffmpeg -re -i input.mp4 -c copy -f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer -frag_duration 1000 - | moq --connect https://relay.example.com/anon --broadcast my-stream.hang import fmp4
moq --connect https://relay.example.com/anon --broadcast my-stream.hang export hls --listen '[::]:8089'

# Record into ./recording, ending once the broadcast finishes
moq --connect https://relay.example.com/anon --broadcast my-stream.hang export hls --output ./recording

# Import
moq --connect https://relay.example.com/anon --broadcast my-stream.hang import hls https://example.com/live/master.m3u8
```

The CLI serves the one `--broadcast` it was given, at
`/{broadcast}/master.m3u8` and `/{broadcast}/manifest.mpd`.

Import follows a live playlist, joining a few segments behind its live edge,
until every rendition's playlist ends with `EXT-X-ENDLIST`. It then keeps the
finished broadcast up for `--linger`
(see [retention](/bin/cli#retention-and-latency)). A playlist that has already
ended, such as a VOD recording, is refused with an error rather than imported.
An `EXT-X-DISCONTINUITY` keeps the source's timestamps. Timestamp resets are not
supported: if a rendition's timestamps go backwards under the same `EXT-X-MAP`,
such as an encoder restart, the import fails with an error. Import the restarted
source as a new broadcast. Import fetches only from the playlist's origin
(scheme, host, and port): a reference or redirect to another origin, such as a
separate CDN host, fails the import. An `EXT-X-BYTERANGE` segment needs an
origin that honors HTTP `Range`: any answer but a `206` for exactly the
requested bytes fails the segment.

## How export works

Export reads each rendition's media track and numbers its groups into HLS
segments on a grid the catalog's [join interval](/concept/hang#catalog)
fixes. A segment is the smallest whole multiple of `media.join.interval` that
is at least a second long, so a 2 s GOP gives 2 s segments, an audio-only
broadcast's 20 ms groups pack into 1 s ones, and a 300 ms interval gives
1.2 s ones. The grid draws a line at every multiple of that length in
presentation time.
A segment request fetches exactly the groups it covers from the relay's cache
and transmuxes them to CMAF. Media is never transcoded, so the player must
support the publisher's codecs.

What that means for you:

- **The publisher declares the interval.** Only a broadcast whose catalog carries `media.join.interval` gets playlists. Rust publishers built on `moq-mux` (the CLI, the RTMP, SRT, and WebRTC gateways, and the native bindings) declare it: an encoder that knows its GOP passes it as a hint, and an ingest without one measures each track over its first second or group, whichever is longer, then declares the longest. `@moq/publish` declares its keyframe interval.
- **Every rendition shares the grid.** A group spans from its first frame's timestamp to the next group's and belongs to the first grid line it contains, or, containing none, the line it starts after. Segment N therefore starts at the group containing N times the segment length, every rendition numbers its segments alike, and each cuts its boundaries at its own keyframes. A grid line no group starts a segment at, because a GOP ran past it or the content paused, is listed as an `EXT-X-GAP` so the numbering never skips: on a 2 s grid, a GOP from 0 to 7 s is segment 0, segments 1 and 2 are gaps, and the next GOP starts segment 3. Timestamps that go back start a discontinuity on that rendition alone; `EXT-X-PROGRAM-DATE-TIME`, written after every discontinuity when the catalog has a clock, lets players resync.
- **Contribution media starts on selection.** The HTTP multivariant playlist reads each root `contributions` entry's catalog and lists its audio and video beside the broadcast's own. A contribution shares the broadcast's timeline: its renditions are segmented on the broadcast's join interval and clock, whatever its own catalog declares, so publish it on the same timestamps. One not yet routed is listed once its broadcast is announced. Reading its catalog starts no encoding: only a request to one of its rendition URLs subscribes to that rendition's media. Demand lasts through active responses and, after the last one finishes, for three target durations or the playlist window plus one target duration, whichever is longer (20 s by default); a later request starts it again. The broadcast's own renditions stay subscribed. A segment is servable for as long as the relay's [cache](/bin/relay/config#cache) retains it. Keep `--window` (the playlist length, default 16 s) within that retention and within the media's max age (30 s by default), after which a superseded segment's groups are stale.
- **Live joins cost the window.** The exporter subscribes 30 s behind the live edge and lists at most 256 segments. One joining partway through a segment lists nothing before the first segment it sees start.
- **Segments are timed by the grid.** A segment is listed once a later one starts or the track ends. It starts at its first group's timestamp, or at its line when that group starts after it (as one resuming from a pause does), or a gap at its line, and lasts until the next segment starts, so a segment's bytes never change with a catalog estimate. A GOP longer than the segment is listed at its first line, so its duration understates its media.
- **The target duration is twice the segment.** Every media playlist keeps one `EXT-X-TARGETDURATION` while its rendition stays in the catalog, even across a reconfigure: twice the segment length, rounded up to whole seconds (twice the interval for a GOP of a second or more, 2 s for 20 ms audio groups), since the GOP containing a grid line starts at most one interval before it, and a gap ends under two segment lengths after its line. Only a final GOP longer than that is listed past it, against RFC 8216's rule: hls.js plays it, but Apple's `mediastreamvalidator` flags it, so keep keyframe intervals within the declared interval.
- **A reconfigure keeps the playlist.** A rendition whose decoder config changes (its resolution, say) keeps its media playlist: the segments already listed stay, under the init they were muxed for, and the new config's segments follow behind an `EXT-X-DISCONTINUITY` and a new `EXT-X-MAP`, as RFC 8216 requires. The DASH manifest lists only the new config's segments.
- **Disabled renditions are hidden.** A rendition the catalog marks `enabled: false` leaves the multivariant playlist and DASH manifest until it re-enables. Its media playlist keeps serving, so a player already on it is not ended with `EXT-X-ENDLIST`.
- **URLs name the stream.** Every URL the multivariant playlist and DASH manifest list sits under `@<instance>`, the stream's instance, so every edge and every reload derive the same URLs and a CDN in front of export caches them. A restarted publisher gets URLs of its own, whose numbering starts over. The multivariant playlist and manifest switch to it at once, while the old stream's playlists end at the segments they listed before the switch, even if its publisher keeps sending; a player already on it plays those out and then has to reload the multivariant playlist. A stream that reached export over moq-lite-06 or older, or moq-transport, has an instance its session minted, so only that export serves its URLs.
- **Only a clean finish ends a playlist.** `EXT-X-ENDLIST` and a `static` DASH manifest mean the media tracks finished, or, on a replaced stream's media playlists, that another publisher took the broadcast over. A rendition whose media track fails instead, such as a publisher dropping its session, answers HTTP 500 with `Cache-Control: no-store`, so a truncated recording never looks complete, and leaves the multivariant playlist and DASH manifest so players switch to a sibling. With none left, the DASH manifest answers the error too.

Contribution URLs nest under the served broadcast's (see
[`Contribution`](https://docs.rs/moq-hls/latest/moq_hls/export/struct.Contribution.html)), so
access middleware sees the parent broadcast's path, while the server resolves each
[contribution](/draft/moq-hang#contributions) through its own scoped origin and leaves out
one it may not read. They follow contribution restarts unless the entry pins an `instance`;
an unavailable contribution leaves the multivariant playlist until it returns. DASH and
`--output` include only the broadcast's own renditions; record a contribution as its own
broadcast.

A [broadcast archive](/concept/archives) is served the same
way, with no second stored copy: record a broadcast, republish it, and export
the republished broadcast. Through this project's publishers and relays,
recording needs moq-lite 07 on every network hop for track discovery; see
[Archives](/concept/archives#discover-and-record) for how each hop opts in.

```bash
moq --connect https://relay.example.com/anon --connect-version moq-lite-07-wip --broadcast event export archive s3://recordings
moq --connect https://relay.example.com/anon --broadcast event-replay.hang import archive s3://recordings/event/@<instance>
moq --connect https://relay.example.com/anon --broadcast event-replay.hang export hls --listen '[::]:8089'
```

The archived catalog and media replay as recorded, so the export lists the same
window as a live broadcast.

## Record

`--output <dir>` writes `master.m3u8` and each rendition's
`{video,audio}/{name}/media.m3u8`, init, and `.m4s` segments into an empty or
absent directory as each segment completes. Recording alone binds no HTTP port;
add `--listen` to serve and record from one stage, in which case serving goes on
after the recording completes.

The recording starts at the oldest segment in the `--window` and gets
`EXT-X-ENDLIST` only once the broadcast finishes cleanly or another publisher
takes it over, in which case it ends with the segments listed before the
takeover. If the publisher drops out, its timestamps go back, or a segment
can't be fetched, the export exits with an error and leaves the playlists
unended. A rendition whose decoder is reconfigured keeps one playlist,
switching to a new init at an `EXT-X-DISCONTINUITY` in that playlist, so the
whole recording stays playable from its master playlist. Each playlist's
`EXT-X-TARGETDURATION` is the one the export serves for that rendition when the recording first sees it (see the
target duration rule above), and never changes, even if the rendition is
removed and re-added with another target, which logs a warning. A GOP longer
than the target is still recorded, with a warning, so keep keyframe intervals
within the declared interval.

`--listen-tls-cert`/`--listen-tls-key` or `--listen-tls-generate` serve HTTPS,
and `--cors-origin` opens it to browsers. HLS TLS reads `MOQ_HLS_TLS_*`;
`MOQ_LISTEN_TLS_*` configures only the MoQ listener. See `moq export hls --help`.

## Import

Import handles classic HLS; LL-HLS parts are not implemented. It publishes the
playlist's own media times, and anchors the catalog clock on the first imported
segment. Each fMP4 segment is split into one frame per sample; `--cmaf`
publishes each fragment whole as CMAF instead, keeping its exact sample timing.

The library, including the recorder cursors for mirroring a broadcast to
storage, is [`moq-hls`](https://docs.rs/moq-hls).

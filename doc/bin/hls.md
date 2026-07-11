---
title: moq-hls
description: HLS <-> MoQ gateway
---

# moq-hls

`moq-hls` bridges [HLS](https://datatracker.ietf.org/doc/html/rfc8216) and Media
over QUIC, in both directions:

- **export**: serve a MoQ broadcast as HLS over HTTP, fetching media on demand.
- **import**: pull a remote HLS master/media playlist and publish it into MoQ.

All CMAF byte handling lives in `moq-mux` (import via its fMP4 importer, export
via its one-shot fMP4 muxer). `moq-hls` owns the HLS manifest generation, the
timeline-driven playlist window, and the HTTP surface. HLS isn't a symmetric
push/pull protocol like WHIP/WHEP, so `moq-hls` uses explicit `export` /
`import` subcommands rather than the `server`/`client` x `publish`/`subscribe`
matrix of `moq-rtc`.

## How export works

The export server never subscribes to media. Per broadcast it subscribes to
exactly two kinds of metadata track:

- the **catalog**, which lists the renditions and their codec configs, and
- each rendition's **timeline** track, a tiny log mapping every media group to
  its start timestamp (the `timeline` section on the rendition's catalog entry
  names it).

Media playlists are rendered purely from the timeline: each record starts a
segment, and the gap to the next record is its duration. Media bytes move only
when a player requests a segment: the server FETCHes exactly the groups that
segment covers from the relay's cache and transmuxes them to CMAF (one
moof+mdat per group). Renditions without a timeline track can't be served this
way and are skipped.

`EXT-X-TARGETDURATION` is the longest segment gap observed in the timeline
window (the timeline is the only cadence signal). When the timeline advertises
a wall-clock anchor, the playlist carries `EXT-X-PROGRAM-DATE-TIME`.

One server is path-based, so it can expose many broadcasts at once:

```text
GET /{broadcast}/master.m3u8
GET /{broadcast}/{kind}/{rendition}/media.m3u8
GET /{broadcast}/{kind}/{rendition}/init.mp4
GET /{broadcast}/{kind}/{rendition}/seg/{group}.m4s
```

`{kind}` is `video` or `audio`, so a video and an audio rendition that share a
name address distinct resources. A segment is addressed by its starting group
sequence; audio timelines may skip groups (they're throttled to about one
record per second of media), in which case one segment covers the whole group
range up to the next record.

## CLI shape

The gateway ships as `moq import hls` / `moq export hls` (see
[moq-cli](/bin/cli)):

```bash
# export: expose a MoQ broadcast as HLS over HTTP
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    export hls --listen '[::]:8089'

# then point a player at the broadcast:
#   http://localhost:8089/my-stream.hang/master.m3u8

# import: pull a remote HLS playlist into MoQ
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    import hls https://example.com/live/master.m3u8
```

### `export hls` flags

- `--listen`: HTTP bind address (default `[::]:8089`).
- `--tls-cert` / `--tls-key`: serve HTTPS from a cert/key pair on disk. Most
  players require HTTPS. `--tls-generate <hostname>` instead generates a
  self-signed cert, and `--server-tls-root` enables optional mTLS client auth.
- `--window`: minimum duration of media listed per rendition playlist (default
  `16s`, humantime syntax). Keep it within the relay's group-cache retention,
  since segments are fetched from there on request.
- `--cors-origin`: allow cross-origin browser access (repeatable, or `'*'`).

## Notes and limitations

- **Relay retention bounds the window.** A segment is only servable while its
  groups are still in the relay's cache (the `[cache]` capacity on
  `moq-relay`). A request for an evicted segment returns 404; players skip
  ahead.
- **Codecs.** Video renditions are served as CMAF; H.264/H.265 Annex-B sources
  are converted to length-prefixed (avc1/hvc1), with inline parameter sets
  resolved from the fetched keyframe. Audio renditions (AAC, Opus) get their
  own media playlist in an `AUDIO` group.
- **Import** currently handles classic HLS (no LL-HLS partial segments on the
  import side) and prefers H.264 renditions.
- **LL-HLS parts and DASH** are not implemented yet. Parts need sub-group
  records in the timeline; an MPD generator can be added over the same
  timeline + FETCH machinery later.

(Written by Claude)

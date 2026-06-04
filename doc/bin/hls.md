---
title: moq-hls
description: HLS / LL-HLS <-> MoQ gateway
---

# moq-hls

`moq-hls` bridges [HLS](https://datatracker.ietf.org/doc/html/rfc8216) (and
Low-Latency HLS) and Media over QUIC, in both directions:

- **serve**: subscribe to a MoQ broadcast and serve HLS + LL-HLS over HTTP.
- **ingest**: pull a remote HLS master/media playlist and publish it into MoQ.

All CMAF byte handling lives in `moq-mux` (import via its fMP4 importer, export
via its fMP4 exporter). `moq-hls` owns the HLS manifest generation, the
segment/part windowing, and the HTTP surface. HLS isn't a symmetric push/pull
protocol like WHIP/WHEP, so `moq-hls` uses explicit `serve` / `ingest`
subcommands rather than the `server`/`client` x `publish`/`subscribe` matrix of
`moq-rtc`.

## How egress works

Each rendition in the broadcast's catalog gets its own
[`moq-mux` fMP4 exporter](/lib/rs/), narrowed to that single track. The exporter
emits CMAF fragments; with a part target set, each GOP is split into LL-HLS
*parts*, and a new keyframe (independent fragment) starts a new *segment*. A
bounded sliding window of segments/parts per rendition backs the playlists.

One server is path-based, so it can expose many broadcasts at once:

```text
GET /{broadcast}/master.m3u8
GET /{broadcast}/{rendition}/media.m3u8   # LL-HLS blocking reload via ?_HLS_msn=&_HLS_part=
GET /{broadcast}/{rendition}/init.mp4
GET /{broadcast}/{rendition}/seg/{seq}.m4s
GET /{broadcast}/{rendition}/part/{seq}/{idx}.m4s
```

The media playlist advertises `EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES`,
`EXT-X-PART-INF`, per-part `EXT-X-PART`, and an `EXT-X-PRELOAD-HINT` for the
next part at the live edge. Blocking-reload and preload-hint requests park until
the requested part lands (or a timeout fires), driven by an internal notify on
each new part.

## CLI shape

```bash
# serve: expose MoQ broadcasts as HLS / LL-HLS over HTTP
moq-hls --relay https://relay.example.com \
        serve --listen 0.0.0.0:8089 --part-target-ms 500

# then point a player at a broadcast:
#   http://localhost:8089/my-stream/master.m3u8

# ingest: pull a remote HLS playlist into MoQ as "cam0"
moq-hls --relay https://relay.example.com \
        ingest --broadcast cam0 --playlist https://example.com/cam0/master.m3u8
```

### Global flags

- `--relay`: upstream MoQ relay to publish into (ingest) or read from (serve).

### `serve` flags

- `--listen`: HTTP bind address (default `[::]:8089`).
- `--tls-cert` / `--tls-key`: serve HTTPS instead. Most players require it.
- `--part-target-ms`: LL-HLS part target duration in milliseconds (default 500).
  This also caps the exporter's fragment duration.
- `--window`: number of segments kept per rendition (default 8).

### `ingest` flags

- `--broadcast`: broadcast name to publish on the relay.
- `--playlist`: remote HLS playlist URL (`http`/`https`) or a local file path.

## Notes and limitations

- **Group skipping.** Egress reads through `moq-mux`'s latency-bounded consumer,
  which can skip stalled GOPs under its budget. `moq-hls` uses a generous budget
  so live GOPs aren't dropped; a lost GOP simply becomes a gap.
- **Codecs.** Video renditions are exported as CMAF; H.264/H.265 Annex-B sources
  are converted to length-prefixed (avc1/hvc1) by the exporter. Audio renditions
  (AAC, Opus) get their own media playlist in an `AUDIO` group.
- **Ingest** currently handles classic HLS (no LL-HLS partial segments on the
  ingest side) and prefers H.264 renditions.
- **DASH** is not implemented yet; the segment store is format-agnostic, so an
  MPD generator can be added over the same parts later.

(Written by Claude)

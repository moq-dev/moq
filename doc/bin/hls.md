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
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang export hls --listen '[::]:8089'

# Import
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang import hls https://example.com/live/master.m3u8
```

Export never subscribes to media. It reads each rendition's
[timeline track](/concept/hang#catalog) to build playlists, then fetches
exactly the groups a requested segment covers from the relay's cache and
transmuxes them to CMAF on demand. So a segment is servable for as long as the
relay's [cache](/bin/relay/config#cache) retains it, and idle renditions cost
nothing. One server exposes every broadcast by path:

```text
/{broadcast}/master.m3u8
/{broadcast}/{video|audio}/{rendition}/media.m3u8
/{broadcast}/{video|audio}/{rendition}/init.mp4
/{broadcast}/{video|audio}/{rendition}/seg/{group}.m4s
```

`--window` sets the playlist duration (default 16 s), `--tls-cert`/`--tls-key`
or `--tls-generate` serve HTTPS, and `--cors-origin` opens it to browsers.
H.264/H.265 and AAC/Opus renditions are served. Import handles classic HLS;
LL-HLS parts and DASH output are not implemented yet. The library is
[`moq-hls`](https://docs.rs/moq-hls).

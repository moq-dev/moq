# moq-srt

SRT gateway for Media over QUIC, both directions.

SRT carries MPEG-TS. This crate runs an SRT listener and routes each connection
by its stream id `m=` mode:

- `m=publish` (the default): ingest. Demux the connection's transport stream
  with [`moq-mux`](../moq-mux) and publish it into a MoQ origin as an ordinary
  broadcast. The contribution-ingest analogue of `moq-hls`'s import and
  `moq-rtc`'s WHIP.
- `m=request`: egress. Re-mux a broadcast from the origin back to MPEG-TS and
  stream it to the caller, so `vlc srt://...` and `ffmpeg -i srt://...` can play
  any broadcast the origin carries (H.264/H.265 video, AAC/AC-3/MP2 audio).

Pure Rust: SRT is provided by `srt-tokio`, with no libsrt or ffmpeg dependency.

## Library

The whole surface is `Config` + `run`. A relay embeds ingest by calling `run`
against its own origin, so the ingested media is published locally with no extra
hop. Depend on it with `default-features = false` to skip the binary's relay
client/server and CLI dependencies:

```toml
moq-srt = { version = "0.0.1", default-features = false }
```

```rust
let mut srt = moq_srt::Config::default();
srt.listen = Some("0.0.0.0:9000".parse()?);
srt.prefix = "live/".to_string();

// `origin` is your relay's local origin (e.g. `cluster.origin.clone()`).
tokio::select! {
    res = moq_srt::run(origin, srt) => res?,
    // ... your relay's accept loop, web server, etc.
}
```

## Binary

The `moq-srt` binary (needs the default `server` feature) has two modes.

`serve` ingests SRT and serves it directly as a local relay, so MoQ subscribers
(native or browser) connect straight to this binary. It also exposes the
`/certificate.sha256` endpoint browsers need for self-signed `http://` origins,
and can serve a static player directory with `--dir`:

```bash
moq-srt serve --server-bind [::]:443 --tls-generate localhost \
  --srt-listen 0.0.0.0:9000 --srt-prefix live/
```

`publish` instead forwards every ingested broadcast out to a remote relay over
WebTransport (like `moq-hls import`):

```bash
moq-srt publish --relay https://relay.example.com \
  --srt-listen 0.0.0.0:9000 --srt-prefix live/
```

Feed either mode with any SRT source:

```bash
# Publish: lands at broadcast `live/cam0`.
ffmpeg -re -i input.mp4 -c copy -f mpegts \
  'srt://127.0.0.1:9000?streamid=#!::r=cam0,m=publish'

# Request: play `live/cam0` back out as MPEG-TS.
ffplay 'srt://127.0.0.1:9000?streamid=#!::r=cam0,m=request'
vlc    'srt://127.0.0.1:9000?streamid=#!::r=cam0,m=request'
```

A request waits for the broadcast to be announced, so a player may connect before
the publisher does.

## Routing

Each connection's broadcast path and direction come from its SRT stream id:

- Standard form `#!::r=<resource>,m=<mode>` -> `<resource>`, with `m=request`
  selecting egress and anything else (including absent) selecting ingest.
- Otherwise the raw stream id (e.g. OBS-style `app/key`), always ingest.

`--srt-prefix` is prepended to namespace a listener's streams. First publisher on
a path wins; a second publish of the same path is rejected. Requests don't claim
a path, so any number of players can pull the same broadcast.

## Auth

The listener is currently unauthenticated: anyone who can reach the UDP port can
publish or request any broadcast. Gate it with a host firewall or a private
network. SRT passphrase encryption and token checks are the planned next step.

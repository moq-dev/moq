# moq-srt

SRT gateway for Media over QUIC, both directions.

SRT carries MPEG-TS. This crate runs an SRT listener and routes each connection
by its stream id `m=` mode:

- `m=publish` (the default): ingest. Demux the connection's transport stream
  with [`moq-mux`](../moq-mux) and publish it into a MoQ origin as an ordinary
  broadcast. The contribution-ingest analogue of `moq-cli` HLS import and
  `moq-rtc`'s WHIP.
- `m=request`: egress. Re-mux a broadcast from the origin back to MPEG-TS and
  stream it to the caller, so `vlc srt://...` and `ffmpeg -i srt://...` can play
  any broadcast the origin carries (H.264/H.265 video, AAC/AC-3/MP2 audio).

Pure Rust: SRT is provided by `srt-tokio`, with no libsrt or ffmpeg dependency.

## Library

Two entry points. `Config` + `run` is the unauthenticated convenience: a relay
embeds ingest by calling `run` against its own origin, so the ingested media is
published locally with no extra hop. For auth, drive `Server` / `Request`
directly (see [Auth](#auth) below).

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

## CLI

A command-line interface is provided by the [`moq-cli`](../moq-cli) binary, on
top of this library.

Feed any SRT source:

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

`run` is unauthenticated: anyone who can reach the UDP port can publish or
request any broadcast. Gate it with a host firewall or a private network, or
bring your own auth by driving `Server` / `Request` directly, mirroring
`moq-native`'s `Server` / `Request`:

```rust
let mut server = moq_srt::Server::bind("0.0.0.0:9000".parse()?, None).await?;
while let Some(request) = server.accept().await {
    // Inspect `request.resource()` / `request.stream_id()` (treat the stream id
    // as a token if you like), verify it, and pick the broadcast path.
    match request {
        moq_srt::Request::Publish(publish) => {
            tokio::spawn(publish.accept(&origin, "live/cam0"));
        }
        moq_srt::Request::Subscribe(subscribe) => {
            tokio::spawn(subscribe.accept(&consumer, "live/cam0"));
        }
    }
    // ...or `request`'s `reject()` to deny it.
}
```

SRT passphrase encryption is a separate, planned next step.

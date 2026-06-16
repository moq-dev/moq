# moq-srt

SRT contribution ingest gateway for Media over QUIC.

SRT carries MPEG-TS. This crate runs an SRT listener, demuxes each connection's
transport stream with [`moq-mux`](../moq-mux), and publishes the result into a
MoQ origin as ordinary broadcasts. It's the contribution-ingest analogue of
`moq-hls`'s import and `moq-rtc`'s WHIP. Pure Rust: SRT is provided by
`srt-tokio`, with no libsrt or ffmpeg dependency.

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
# Lands at broadcast `live/cam0`.
ffmpeg -re -i input.mp4 -c copy -f mpegts \
  'srt://127.0.0.1:9000?streamid=#!::r=cam0,m=publish'
```

## Routing

Each connection's broadcast path comes from its SRT stream id:

- Standard form `#!::r=<resource>,m=publish` -> `<resource>`.
- Otherwise the raw stream id (e.g. OBS-style `app/key`).

`--srt-prefix` is prepended to namespace a listener's streams. First publisher on
a path wins; a second connection to the same path is rejected.

## Auth

The listener is currently unauthenticated: anyone who can reach the UDP port can
publish. Gate it with a host firewall or a private network. SRT passphrase
encryption and token checks are the planned next step.

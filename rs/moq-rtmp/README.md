# moq-rtmp

RTMP / enhanced-RTMP gateway for Media over QUIC: contribution ingest and egress.

RTMP carries FLV-format audio/video. This crate runs an RTMP server that bridges
both directions with [`moq-mux`](../moq-mux): on **publish** it re-wraps a
client's messages as FLV tags, demuxes them, and publishes the result into a MoQ
origin as ordinary broadcasts; on **play** it subscribes to a broadcast from the
origin, muxes it back to FLV, and streams the tags down to the player. It's the
sibling of `moq-srt`, `moq-cli` HLS import/export, and `moq-rtc`'s WHIP/WHEP. Both
legacy RTMP (H.264 + AAC) and enhanced RTMP (E-RTMP: HEVC, AV1, VP9, Opus, AC-3)
work in each direction, since the codec handling lives in the `moq-mux` FLV
demuxer/muxer. Pure Rust: the protocol is provided by `rml_rtmp`, with no librtmp
or ffmpeg dependency.

## Library

RTMPS (`Config::tls` / `Server::with_tls`) is the only optional piece; drop it
with `default-features = false` for a plaintext-only build:

```toml
moq-rtmp = { version = "0.0.1", default-features = false }
```

There are two entry points.

### `run` (unauthenticated)

`Config` + `run` accepts every publisher and player and routes by prefix +
app/key (publishers ingest into the origin, players are served out of it). A relay
embeds the gateway by calling `run` against its own origin, so the media stays
local with no extra hop:

```rust
let mut rtmp = moq_rtmp::Config::default();
rtmp.listen = Some("0.0.0.0:1935".parse()?);
rtmp.prefix = "live/".to_string();

// `origin` is your relay's local origin (e.g. `cluster.origin.clone()`).
tokio::select! {
    res = moq_rtmp::run(origin, rtmp) => res?,
    // ... your relay's accept loop, web server, etc.
}
```

### `Server` / `Request` (bring your own auth)

To gate access, drive the `Server` directly. `accept` runs the handshake and the
connect exchange, then yields a `Request` once the client wants to publish or
play. The `Request` is either a `Publish` or a `Play`; you inspect the app and
stream key, make a decision, and `accept` or `reject` it. This mirrors
`moq-native`'s `Server` / `Request`, so there's no callback: the auth policy lives
in your loop.

```rust
let mut server = moq_rtmp::Server::bind("0.0.0.0:1935".parse()?).await?;
let consumer = origin.consume(); // players are served out of this
while let Some(request) = server.accept().await {
    let origin = origin.clone();
    let consumer = consumer.clone();
    // Spawn per connection: `accept` pumps media for the whole connection, so
    // handling it inline would serialize clients.
    tokio::spawn(async move {
        // Treat the stream key as a token (e.g. a moq-token JWT) and the app as
        // the broadcast path. Verify however you like; the origin can be scoped
        // per token with `with_root` / `scope`.
        match request {
            moq_rtmp::Request::Publish(publish) => match authorize(publish.app(), publish.stream_key()).await {
                Ok(path) => { let _ = publish.accept(&origin, &path).await; }
                Err(err) => { let _ = publish.reject(&err.to_string()).await; }
            },
            moq_rtmp::Request::Play(play) => match authorize(play.app(), play.stream_key()).await {
                Ok(path) => { let _ = play.accept(&consumer, &path).await; }
                Err(err) => { let _ = play.reject(&err.to_string()).await; }
            },
        }
    });
}
```

### RTMPS (RTMP over TLS)

Two ways to serve `rtmps://`:

- **Let the gateway terminate TLS.** Set `Config::tls` (or call
  `Server::with_tls`) with a `rustls::ServerConfig`, and the listener speaks
  RTMPS with no other change. Build the config from a `moq_native::tls::Server`
  instance (RTMPS has no ALPN), or supply any `rustls::ServerConfig`. To serve
  both RTMP and RTMPS, run two listeners (`run` once per config) against a
  cloned origin.

  ```rust
  let mut tls = moq_native::tls::Server::default();
  tls.generate = vec!["your-domain.com".to_string()]; // or set tls.cert / tls.key
  let server_config = tls.server_config(vec![])?; // RTMPS has no ALPN

  let mut rtmps = moq_rtmp::Config::default();
  rtmps.listen = Some("0.0.0.0:443".parse()?);
  rtmps.tls = Some(server_config); // Arc<rustls::ServerConfig>
  ```

- **Bring your own transport.** Accept the connection and complete the TLS
  handshake yourself (or use any other `AsyncRead + AsyncWrite` stream: a proxy
  socket, a test pipe), then hand the established stream to `accept_stream`,
  which runs the RTMP handshake and yields the same `Request`:

  ```rust
  let tls = acceptor.accept(tcp).await?; // your tokio_rustls TlsAcceptor
  if let Some(request) = moq_rtmp::accept_stream(tls, peer).await? {
      // authorize, then match on Request::Publish / Request::Play and accept it
  }
  ```

## CLI

A command-line interface is provided by the [`moq-cli`](../moq-cli) binary, on
top of this library.

Point any RTMP source at the gateway. OBS: set the server to
`rtmp://127.0.0.1:1935/live` and the stream key to `cam0`. ffmpeg:

```bash
# Lands at broadcast `live/cam0`.
ffmpeg -re -i input.mp4 -c copy -f flv rtmp://127.0.0.1:1935/live/cam0

# Enhanced RTMP (HEVC) lands the same way.
ffmpeg -re -i input.mp4 -c:v hevc -c:a aac -f flv rtmp://127.0.0.1:1935/live/cam0
```

Play any broadcast back out over RTMP from a player (VLC, ffplay, mpv):

```bash
# Pulls broadcast `live/cam0` (the same URL it was pushed to).
ffplay rtmp://127.0.0.1:1935/live/cam0
```

## Routing

Each connection's broadcast path is `<app>/<key>`, from the RTMP app and stream
key (`rtmp://host/<app>/<key>`), falling back to just the app when the key is
empty. `--rtmp-prefix` is prepended to namespace a listener's streams. The same
routing applies to both directions, so the URL round-trips. First publisher on a
path wins (a second publish to a live path is rejected); plays don't claim a path,
so any number of players can pull the same broadcast at once, and a play waits for
the broadcast to be announced.

## Auth

The `run` entry point (and the `moq-cli` CLI built on it) is unauthenticated:
anyone who can reach the TCP port can publish or play, so gate them with a host firewall or a
private network. To authenticate, use the `Server` / `Request` API above and
verify each request in your accept loop (e.g. the stream key as a moq-token JWT,
the app as the broadcast path) before accepting it. That is the intended
integration point for a relay that already has JWT/path auth.

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

RTMPS (`Server::with_tls`) is the only optional piece; drop it with
`default-features = false` for a plaintext-only build:

```toml
moq-rtmp = { version = "0.0.1", default-features = false }
```

### `Server` / `Request`

Drive the `Server` against your own origin. `accept` runs the handshake and the
connect exchange, then yields a `Request` once the client wants to publish or
play. The `Request` is either a `Publish` or a `Play`; you inspect the app and
stream key, make a decision, and `accept` or `reject` it. This mirrors
`moq-tokio`'s `Server` / `Request`, so there's no callback: the routing and auth
policy live in your loop.

```rust
let mut server = moq_rtmp::Server::bind("0.0.0.0:1935".parse()?).await?;
let consumer = origin.consume(); // players are served out of this
while let Some(request) = server.accept().await {
    let origin = origin.clone();
    let consumer = consumer.clone();
    // Spawn per connection: `accept` pumps media for the whole connection, so
    // handling it inline would serialize clients.
    tokio::spawn(async move {
        // Treat the stream key as a token (e.g. a moq-auth JWT) and the app as
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

- **Let the gateway terminate TLS.** Call `Server::with_tls` with a
  `rustls::ServerConfig`, and the listener serves RTMPS alongside plaintext RTMP
  on the same port: a client that opens with a TLS ClientHello is
  TLS-terminated, any other is served as plaintext. Build the config from a
  `moq_tokio::tls::Listen` instance (RTMPS has no ALPN), or supply any
  `rustls::ServerConfig`. Call `Server::with_plaintext(false)` to serve RTMPS
  only: a plaintext client is refused at its first byte, so stream keys never
  cross the network unencrypted. Refusing plaintext without a TLS config makes
  `accept` refuse to run.

  ```rust
  let mut tls = moq_tokio::tls::Listen::default();
  tls.generate = vec!["your-domain.com".to_string()]; // or set tls.cert / tls.key
  let server_config = tls.server_config(vec![])?; // RTMPS has no ALPN

  let server = moq_rtmp::Server::bind("0.0.0.0:1935".parse()?)
      .await?
      .with_tls(server_config) // Arc<rustls::ServerConfig>: rtmp:// and rtmps://
      .with_plaintext(false); // optional: rtmps:// only
  ```

- **Bring your own transport.** Accept the connection and complete the TLS
  handshake yourself (or use any other `AsyncRead + AsyncWrite` stream: a proxy
  socket, a test pipe), then hand the established stream to `accept_stream`,
  which runs the RTMP handshake and yields the same `Request`:

  ```rust
  moq_rtmp::configure_socket(&tcp, peer); // Nagle off, keepalive on
  let tls = acceptor.accept(tcp).await?; // your tokio_rustls TlsAcceptor
  if let Some(request) = moq_rtmp::accept_stream(tls, peer).await? {
      // authorize, then match on Request::Publish / Request::Play and accept it
  }
  ```

  When the transport is a `TcpStream` you own, call `configure_socket` on it
  first; `Server` does this for every socket it accepts. Its keepalive is the
  only thing that reaps a **play** session whose viewer vanished without a FIN:
  publishers are bounded by `PUBLISH_IDLE_TIMEOUT`, but a play session sitting on
  a quiet broadcast neither reads nor writes, so nothing else would notice.

## CLI

A command-line interface is provided by the [`moq-cli`](../moq-cli) binary, on
top of this library. Its listener bridges the single `--broadcast` and ignores
the RTMP app and stream key; see [`doc/bin/rtmp.md`](../../doc/bin/rtmp.md).

## Auth

The library authenticates nothing on its own: your accept loop decides. Verify
each request (e.g. the stream key as a moq-auth JWT, the app as the broadcast
path) before accepting it, and scope the origin per token. Any number of players
can play the same path, and a play waits for its broadcast to be announced.

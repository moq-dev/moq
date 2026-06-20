# moq-rtmp

RTMP / enhanced-RTMP contribution ingest gateway for Media over QUIC.

RTMP carries FLV-format audio/video. This crate runs an RTMP server, re-wraps
each connection's messages as FLV tags, demuxes them with
[`moq-mux`](../moq-mux), and publishes the result into a MoQ origin as ordinary
broadcasts. It's the contribution-ingest analogue of `moq-srt`, `moq-hls`'s
import, and `moq-rtc`'s WHIP. Both legacy RTMP (H.264 + AAC) and enhanced RTMP
(E-RTMP: HEVC, AV1, VP9, Opus, AC-3) work, since the codec handling lives in the
`moq-mux` FLV demuxer. Pure Rust: the protocol is provided by `rml_rtmp`, with no
librtmp or ffmpeg dependency.

## Library

Depend on it with `default-features = false` to skip the binary's relay
client/server and CLI dependencies:

```toml
moq-rtmp = { version = "0.0.1", default-features = false }
```

There are two entry points.

### `run` (unauthenticated)

`Config` + `run` accepts every publisher and routes by prefix + app/key. A relay
embeds ingest by calling `run` against its own origin, so the ingested media is
published locally with no extra hop:

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

To gate ingest, drive the `Server` directly. `accept` runs the handshake and the
connect/publish exchange, then yields a `Request` once the client wants to
publish. You inspect the app and stream key, make a decision, and `accept` or
`reject` it. This mirrors `moq-native`'s `Server` / `Request`, so there's no
callback: the auth policy lives in your loop.

```rust
let mut server = moq_rtmp::Server::bind("0.0.0.0:1935".parse()?).await?;
while let Some(request) = server.accept().await {
    let origin = origin.clone();
    // Spawn per connection: `accept` pumps media for the whole connection, so
    // handling it inline would serialize publishers.
    tokio::spawn(async move {
        // Treat the stream key as a token (e.g. a moq-token JWT) and the app as
        // the broadcast path. Verify however you like; on success choose where to
        // publish (`origin` can be scoped per token with `with_root` / `scope`).
        match authorize(request.app(), request.stream_key()).await {
            Ok(path) => { let _ = request.accept(&origin, &path).await; }
            Err(err) => { let _ = request.reject(&err.to_string()).await; }
        }
    });
}
```

### RTMPS (RTMP over TLS)

Two ways to serve `rtmps://`:

- **Let the gateway terminate TLS.** Set `Config::tls` (or call
  `Server::with_tls`) with a `rustls::ServerConfig`, and the listener speaks
  RTMPS with no other change. Build the config from a cert/key with
  `moq_native::tls::Server::server_config(vec![])` (RTMPS has no ALPN), or any
  `rustls::ServerConfig`. To serve both RTMP and RTMPS, run two listeners
  (`run` once per config) against a cloned origin.

  ```rust
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
      // authorize, then request.accept(&origin, &path).await?
  }
  ```

## Binary

The `moq-rtmp` binary (needs the default `server` feature) has two modes.

`serve` ingests RTMP and serves it directly as a local relay, so MoQ subscribers
(native or browser) connect straight to this binary. It also exposes the
`/certificate.sha256` endpoint browsers need for self-signed `http://` origins,
and can serve a static player directory with `--dir`:

```bash
moq-rtmp serve --server-bind [::]:443 --tls-generate localhost \
  --rtmp-listen 0.0.0.0:1935 --rtmp-prefix live/
```

`publish` instead forwards every ingested broadcast out to a remote relay over
WebTransport (like `moq-srt publish` / `moq-hls import`):

```bash
moq-rtmp publish --relay https://relay.example.com \
  --rtmp-listen 0.0.0.0:1935 --rtmp-prefix live/
```

Either mode also accepts RTMPS alongside plaintext RTMP. Add `--rtmps-listen`
with a cert (`--rtmps-tls-cert` / `--rtmps-tls-key`, or `--rtmps-tls-generate`
for a throwaway self-signed cert), and OBS/ffmpeg can publish to `rtmps://`:

```bash
moq-rtmp serve --server-bind [::]:443 --tls-generate localhost \
  --rtmp-listen 0.0.0.0:1935 \
  --rtmps-listen 0.0.0.0:1936 --rtmps-tls-cert cert.pem --rtmps-tls-key key.pem \
  --rtmp-prefix live/
```

Feed either mode with any RTMP source. OBS: set the server to
`rtmp://127.0.0.1:1935/live` and the stream key to `cam0`. ffmpeg:

```bash
# Lands at broadcast `live/cam0`.
ffmpeg -re -i input.mp4 -c copy -f flv rtmp://127.0.0.1:1935/live/cam0

# Enhanced RTMP (HEVC) lands the same way.
ffmpeg -re -i input.mp4 -c:v hevc -c:a aac -f flv rtmp://127.0.0.1:1935/live/cam0
```

## Routing

Each connection's broadcast path is `<app>/<key>`, from the RTMP app and stream
key (`rtmp://host/<app>/<key>`), falling back to just the app when the key is
empty. `--rtmp-prefix` is prepended to namespace a listener's streams. First
publisher on a path wins; a second connection to the same path is rejected.

## Auth

The `run` entry point and the `moq-rtmp` binary are unauthenticated: anyone who
can reach the TCP port can publish, so gate them with a host firewall or a
private network. To authenticate, use the `Server` / `Request` API above and
verify each publish in your accept loop (e.g. the stream key as a moq-token JWT,
the app as the broadcast path) before calling `request.accept`. That is the
intended integration point for a relay that already has JWT/path auth.

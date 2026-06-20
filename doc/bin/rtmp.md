---
title: moq-rtmp
description: RTMP / enhanced-RTMP <-> MoQ gateway (ingest and egress)
---

# moq-rtmp

`moq-rtmp` bridges [RTMP](https://en.wikipedia.org/wiki/Real-Time_Messaging_Protocol)
(the protocol OBS, ffmpeg, and most hardware encoders and players speak) and
Media over QUIC, in **both directions**:

- **Publish (ingest):** an encoder pushes a stream in, and `moq-rtmp` publishes it
  into MoQ as an ordinary broadcast.
- **Play (egress):** a player pulls `rtmp://host/<app>/<key>`, and `moq-rtmp`
  subscribes to that broadcast from MoQ and streams it back down. VLC, ffplay, and
  mpv can play it (browsers can't -- Flash is dead).

RTMP carries media as FLV-format audio/video messages. `moq-rtmp` runs the RTMP
handshake and chunk/AMF session (via the pure-Rust
[`rml_rtmp`](https://crates.io/crates/rml_rtmp), no librtmp). On ingest it re-wraps
each message as an FLV tag and feeds it to `moq-mux`'s FLV demuxer; on egress it
muxes the broadcast back to FLV with `moq-mux` and sends the tags out as RTMP
messages. It's the sibling of `moq-srt` (SRT/MPEG-TS) and `moq-rtc` (WHIP/WHEP).

Both **legacy RTMP** (H.264 + AAC) and **enhanced RTMP** (E-RTMP: the HEVC, AV1,
VP9, Opus, and AC-3 FourCC payloads) are supported in each direction, because all
codec handling lives in the `moq-mux` FLV demuxer/muxer. Legacy players that speak
only H.264 + AAC will reject the E-RTMP codecs on the play path.

## CLI shape

The binary has two modes, mirroring `moq-srt`:

```bash
# serve: ingest RTMP and serve it directly as a local relay
moq-rtmp serve --server-bind [::]:443 --tls-generate localhost \
  --rtmp-listen 0.0.0.0:1935 --rtmp-prefix live/

# publish: ingest RTMP and forward broadcasts to a remote relay
moq-rtmp publish --relay https://relay.example.com \
  --rtmp-listen 0.0.0.0:1935 --rtmp-prefix live/
```

Point any encoder at it. In OBS, set the server to `rtmp://127.0.0.1:1935/live`
and the stream key to `cam0`; with ffmpeg:

```bash
# Lands at broadcast `live/cam0`.
ffmpeg -re -i input.mp4 -c copy -f flv rtmp://127.0.0.1:1935/live/cam0
```

Then play the same broadcast back out over RTMP from any player:

```bash
# Pulls broadcast `live/cam0` (the same URL it was pushed to).
ffplay rtmp://127.0.0.1:1935/live/cam0
mpv rtmp://127.0.0.1:1935/live/cam0
vlc rtmp://127.0.0.1:1935/live/cam0
```

### `serve` flags

- `--server-bind`: QUIC/WebTransport bind address (default `[::]:443`). Also
  serves the `/certificate.sha256` endpoint browsers need for self-signed
  `http://` origins, and a static player directory with `--dir`.
- `--tls-generate <hostname>` / `--tls-cert` / `--tls-key`: server TLS.

### `publish` flags

- `--relay`: upstream MoQ relay to publish every ingested broadcast into.

### RTMP flags

- `--rtmp-listen`: TCP bind address for the RTMP server (default `[::]:1935`).
- `--rtmp-prefix`: prepended to every broadcast path, to namespace a listener's
  streams (e.g. `live/`).

### RTMPS flags

RTMPS (RTMP over TLS, `rtmps://`) is served on a second listener alongside
plaintext RTMP, sharing the same `--rtmp-prefix`:

- `--rtmps-listen`: TCP bind address for the RTMPS server (off unless set). RTMPS
  has no well-known port; 443 or a custom one are common.
- `--rtmps-tls-cert` / `--rtmps-tls-key`: PEM certificate chain and key.
- `--rtmps-tls-generate <hostname>`: or generate a throwaway self-signed cert
  (testing only; clients must disable verification).

```bash
moq-rtmp serve --server-bind [::]:443 --tls-generate localhost \
  --rtmp-listen 0.0.0.0:1935 \
  --rtmps-listen 0.0.0.0:1936 --rtmps-tls-cert cert.pem --rtmps-tls-key key.pem \
  --rtmp-prefix live/
```

## Routing

Each connection's broadcast path is `<app>/<key>` from the RTMP app and stream
key (`rtmp://host/<app>/<key>`), falling back to just the app when the key is
empty, with `--rtmp-prefix` prepended. The same routing applies to both
directions, so the URL round-trips: push to `rtmp://host/live/cam0`, then pull it
back from `rtmp://host/live/cam0`.

A play waits for the broadcast to be announced, so a player can connect slightly
before the publisher. First **publisher** on a path wins (a second publish to a
live path is rejected); **plays** don't claim a path, so any number of players can
pull the same broadcast at once. In `serve` mode plays are served from the same
origin the server exposes, so anything in it -- RTMP ingests and otherwise -- can
be pulled back out over RTMP.

## Notes and limitations

- **Auth.** The binary (and the `moq_rtmp::run` convenience) is unauthenticated:
  anyone who can reach the TCP port can publish or play. Gate it with a host
  firewall or a private network. To authenticate, embed the library and drive its
  `Server` / `Request` API: `Server::accept` yields a `Request` that is either a
  `Publish` or a `Play`, and you verify the app / stream key (e.g. the stream key
  as a moq-token JWT) before accepting it into / out of an origin at a path of your
  choosing, or rejecting it -- no callback, the policy lives in your loop.
- **Embedding.** A relay can run the gateway in-process by depending on the
  `moq-rtmp` library (`default-features = false`). Call `moq_rtmp::run` against its
  own origin for the unauthenticated case (publishers ingest into it, players are
  served out of it), or use `Server` / `Request` to plug in the relay's existing
  JWT/path auth and scope the origin per token. Either way the media stays local
  with no extra hop.
- **RTMPS.** Embedders can terminate TLS themselves: set `Config::tls` (or
  `Server::with_tls`) with a `rustls::ServerConfig`, or accept the connection and
  finish the TLS handshake by hand and hand the stream to `moq_rtmp::accept_stream`
  (which works over any `AsyncRead + AsyncWrite` transport).
- **Codecs.** FLAC and MP3 enhanced-audio payloads are dropped (no MoQ catalog
  codec); everything else (H.264/HEVC/AV1/VP9 video, AAC/Opus/AC-3/E-AC-3 audio)
  is supported.

(Written by Claude)

---
title: Rust
description: The reference implementation, as a set of crates on crates.io
---

# Rust

The reference implementation. Published crates are on
[crates.io](https://crates.io/search?q=moq) with API docs on docs.rs; source
links cover crates awaiting their first release.

Start with [moq-net](/lib/rs/moq-net) and `moq-tokio` for a connection,
then add [hang](/lib/rs/hang) for media. To run a tool without embedding it,
see [Applications](/bin/). [Examples](/setup/examples) links runnable consumers;
[Repository map](/setup/repository) locates internal crates and their tests.

## Crates

| Crate | Does |
| --- | --- |
| [moq-net](/lib/rs/moq-net) | The pub/sub layer: sessions, origins, broadcasts, tracks, groups, frames. Transport-agnostic. |
| [moq-pattern](https://docs.rs/moq-pattern) | Path patterns such as `room/**`. Re-exported by moq-net and moq-auth. |
| [moq-tokio](https://docs.rs/moq-tokio) | Connects and listens over QUIC, WebSocket, or iroh, from config or CLI flags. Its `driver` feature alone is a tokio QUIC stack without MoQ. |
| [qmux](https://docs.rs/qmux) | QUIC streams, datagrams, and flow control over TCP, TLS, WebSocket, and Unix sockets, on moq-quic's state machine. |
| [moq-sock](https://docs.rs/moq-sock) | Socket binding and CPU pinning for thread-per-core listeners. |
| [moq-ice](https://github.com/moq-dev/moq/tree/main/rs/moq-ice) | Internal infrastructure with no stability promise; the ICE agent behind native WebRTC: drives a str0m connection over UDP, with a STUN client. |
| [moq-uring](https://docs.rs/moq-uring) | Experimental Linux io\_uring server for moq-lite. |
| [hang](/lib/rs/hang) | The media layer: catalog, containers, ordered frame delivery. |
| [moq-mux](/lib/rs/moq-mux) | Import and export fMP4/CMAF, MPEG-TS, Matroska, FLV, and Annex-B. |
| [moq-archive](https://docs.rs/moq-archive) | Record and replay MoQ broadcasts on any `object_store` backend. |
| [moq-video](/lib/rs/moq-video) | Native capture, hardware encode/decode (Apple, Windows, NVIDIA, VAAPI, V4L2, Android), and GPU rendering. |
| [moq-v4l](https://docs.rs/moq-v4l) | Safe Video4Linux 2 bindings. |
| [moq-audio](/lib/rs/moq-audio) | Microphone and speaker, Opus/PCM/AAC codecs, echo cancellation. |
| [moq-transcode](https://docs.rs/moq-transcode) | Just-in-time rendition ladders, GPU-resident on NVIDIA. |
| [moq-auth](/lib/rs/moq-auth) | Relay authorization: JWT signing and verification, and the auth server contract. |
| [moq-room](/lib/rs/moq-room) | Workspace-only native rooms; use the published JS room API for applications. |
| [moq-json](/lib/rs/moq-json) | JSON over tracks: snapshots with merge-patch deltas, or append logs. |
| [moq-e2ee](https://github.com/moq-dev/moq/tree/main/rs/moq-e2ee) | Workspace-only end-to-end encryption of groups, datagrams, and track names, scoped to a publisher epoch. |
| [moq-loc](https://docs.rs/moq-loc), [moq-msf](https://docs.rs/moq-msf) | The IETF LOC container and MSF catalog. |
| [moq-stats](https://docs.rs/moq-stats) | Publish and consume relay traffic counters as tracks. |
| [moq-clock](/lib/rs/moq-clock) | One clock for time and timers on tokio, the browser, and a deterministic test simulator. |
| [moq-hls](https://docs.rs/moq-hls), [moq-rtmp](https://docs.rs/moq-rtmp), [moq-srt](https://docs.rs/moq-srt), [moq-rtsp](https://github.com/moq-dev/moq/tree/main/rs/moq-rtsp), [moq-rtc](https://docs.rs/moq-rtc) | The [gateways](/bin/), as libraries you can embed with your own auth. |
| [moq-ffi](https://docs.rs/moq-ffi), [moq-c](/lib/c/) | The UniFFI core behind the language bindings, and the C ABI. |
| [moq-relay](/bin/relay/), [moq-cli](/bin/cli) | The binaries, also usable as crates. |
| [web-transport](https://github.com/moq-dev/web-transport) | The WebTransport traits and the browser and iroh transports, in a sibling repository. |

## Quick start

`moq-tokio` configures the endpoint; `moq-net` does the protocol.

```rust
// The Origin is the local hub: the session fills it with remote broadcasts
// and serves your local broadcasts out of it.
let origin = moq_tokio::origin::spawn();

let client = moq_tokio::connect::Config::default().init(Default::default())?;
let url = url::Url::parse("https://cdn.moq.dev/anon")?;
// Reconnects on its own; `closed()` resolves when it gives up.
let session = client.with_origin(origin.clone()).connect(url);

// Subscribe: wait for a route, resolve the broadcast at its path, read the catalog.
let consumer = origin.consume();
let mut announced = consumer.announced();
while let Some(event) = announced.next().await {
    // Skip updates and retractions.
    let moq_net::announce::Event::Start(update) = event else { continue };
    let broadcast = consumer.request_broadcast(&update.prefix, update.route.epoch).await?;
    let catalog = broadcast
        .track(hang::Catalog::DEFAULT_NAME)?
        .subscribe(hang::Catalog::default_subscription())
        .await?;
    // moq-mux decodes the catalog; moq-video / moq-audio decode the media.
}
```

```rust
// Publish: create and announce a broadcast on the origin, then fill it.
let mut broadcast = origin.publish("my-stream.hang", Default::default())?;
// moq-mux (from a container) or moq-video / moq-audio (from a device) fill it.
// The route retracts on `unannounce()` or when the broadcast ends. To serve a whole
// subtree on demand instead, `origin.requests("room")?` yields each requested path:
// `prepare(Epoch::mint())`, fill its broadcast, then `publish()`, or reject it.
```

The connection and origin helpers spawn their drivers. Watch
`session.closed()` alongside the announcement loop to handle a connection
that gives up. Before exiting, `session.close().await` delivers what was
already queued, then `Client::close` sends the QUIC close. Runnable examples:
[`rs/hang/examples/video.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs)
(publish) and
[`subscribe.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs).

URLs may be `https://` (QUIC or WebTransport, with a WebSocket fallback),
`moql://`/`moqt://` (raw QUIC), or `iroh://`. Put credentials in
`connect::Config::tokens` rather than the URL: the first rides SETUP (and
`?jwt=` while an offered version needs it), each other widens the grant in band,
and `Connection::tokens().set(...)` replaces them before they expire, with
`answered().await` reporting the relay's verdict. A `?jwt=`
already in the URL stays the connection credential, which reaches an older
relay. `http://` is for a relay on localhost only: it fetches the certificate
fingerprint unauthenticated, so never send a token over it. QUIC tuning shares
the relay's settings; see the [relay reference](/bin/relay/config#quic).

## WebAssembly

`moq-net` compiles to `wasm32-unknown-unknown` and rides the browser's own
`WebTransport`, so Rust logic can be shared between native and web. Skip
`moq-tokio` there. A client handshake returns a session and a separate driver;
spawn `moq_net::time::run(driver)` on the browser executor, and drive the
origin the same way. The [browser transport adapter](https://github.com/moq-dev/moq/blob/main/rs/moq-wasm/src/transport.rs)
and [handshake](https://github.com/moq-dev/moq/blob/main/rs/moq-wasm/src/lib.rs)
show the setup, including advertising the supported MoQ ALPNs. If you just
want MoQ in a page, the [TypeScript libraries](/lib/js/) are the easier path.

## Conventions

The API is producer/consumer pairs at every level (origin, broadcast, track,
group). Producers write and consumers read; cloning a consumer shares the
subscription, and the last clone dropping closes it. The core exposes
executor-agnostic `poll_*` methods with optional async helpers; `moq-tokio`
handles native runtime integration. Errors are typed enums with `thiserror`.

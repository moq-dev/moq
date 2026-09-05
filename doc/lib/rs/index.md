---
title: Rust
description: The reference implementation, as a set of crates on crates.io
---

# Rust

The reference implementation. Every crate is on
[crates.io](https://crates.io/search?q=moq) with API docs on docs.rs.

## Crates

| Crate | Does |
| --- | --- |
| [moq-net](/lib/rs/moq-net) | The pub/sub layer: sessions, origins, broadcasts, tracks, groups, frames. Transport-agnostic. |
| [moq-native](https://docs.rs/moq-native) | Stands up QUIC (quinn, quiche, or noq), TLS, WebSocket fallback, and iroh, from config or CLI flags. |
| [hang](/lib/rs/hang) | The media layer: catalog, containers, ordered frame delivery. |
| [moq-mux](/lib/rs/moq-mux) | Import and export fMP4/CMAF, MPEG-TS, Matroska, FLV, and Annex-B. |
| [moq-video](/lib/rs/moq-video) | Native capture, hardware encode/decode (Apple, Windows, NVIDIA, VAAPI, V4L2, Android), and GPU rendering. |
| [moq-audio](/lib/rs/moq-audio) | Microphone and speaker, Opus/PCM/AAC codecs, echo cancellation. |
| [moq-transcode](https://docs.rs/moq-transcode) | Just-in-time rendition ladders, GPU-resident on NVIDIA. |
| [moq-token](/lib/rs/moq-token) | JWT keys, signing, verification, path authorization. |
| [moq-json](https://docs.rs/moq-json) | JSON over tracks: snapshots with merge-patch deltas, or append logs. |
| [moq-flate](https://docs.rs/moq-flate) | Group-scoped DEFLATE for any track. |
| [moq-loc](https://docs.rs/moq-loc), [moq-msf](https://docs.rs/moq-msf) | The IETF LOC container and MSF catalog. |
| [moq-stats](https://docs.rs/moq-stats) | Publish and consume relay traffic counters as tracks. |
| [moq-hls](https://docs.rs/moq-hls), [moq-rtmp](https://docs.rs/moq-rtmp), [moq-srt](https://docs.rs/moq-srt), [moq-rtc](https://docs.rs/moq-rtc) | The [gateways](/bin/), as libraries you can embed with your own auth. |
| [moq-ffi](https://docs.rs/moq-ffi), [libmoq](/lib/c/) | The UniFFI core behind the language bindings, and the C ABI. |
| [moq-relay](/bin/relay/), [moq-cli](/bin/cli) | The binaries, also usable as crates. |
| [web-transport](https://github.com/moq-dev/web-transport) | The QUIC/WebTransport/qmux transports, in a sibling repository. |

## Quick start

`moq-native` configures the endpoint; `moq-net` does the protocol.

```rust
// The Origin is the local hub: the session fills it with remote broadcasts
// and serves your local broadcasts out of it.
let origin = moq_net::Origin::random().produce();

let client = moq_native::ClientConfig::default().init()?;
let url = url::Url::parse("https://cdn.moq.dev/anon")?;
let session = client.with_subscriber(origin.clone()).with_publisher(&origin).connect(url).await?;

// Subscribe: wait for a broadcast, read its catalog, then its tracks.
let mut announced = origin.consume().announced();
while let Some(update) = announced.next().await {
    let Some(broadcast) = update.broadcast else { continue };
    let catalog = broadcast
        .track(hang::Catalog::DEFAULT_NAME)?
        .subscribe(hang::Catalog::default_subscription())
        .await?;
    // moq-mux decodes the catalog; moq-video / moq-audio decode the media.
}
```

```rust
// Publish: create a broadcast on the origin and add tracks to it.
let route = moq_net::broadcast::Route::new().with_announce(true);
let mut broadcast = origin.create_broadcast("my-stream.hang", route)?;
// moq-mux (from a container) or moq-video / moq-audio (from a device) fill it.
```

The examples run the session and the origin work concurrently (`tokio::select!` or
`spawn`), since the announcement loop is live. Runnable examples:
[`rs/hang/examples/video.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs)
(publish) and
[`subscribe.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs).
URLs may be `https://` (WebTransport, with raw QUIC preferred for native),
`moql://`/`moqt://` (raw QUIC), or `iroh://`. A `?jwt=` query carries the
token. `http://` is for a relay on localhost only: it fetches the certificate
fingerprint unauthenticated before upgrading, so never send a token over it. Connections race
QUIC against WebSocket and remember which won.

## WebAssembly

`moq-net` compiles to `wasm32-unknown-unknown` and rides the browser's own
`WebTransport` through the `web-transport` crate, so Rust logic can be shared
between native and web. Skip `moq-native` there, build the transport with
`web_transport::ClientBuilder`, and drive the returned session future with
`wasm_bindgen_futures::spawn_local` (nothing is `Send` on wasm). If you just
want MoQ in a page, the [TypeScript libraries](/lib/js/) are the easier path.

## Conventions

The API is producer/consumer pairs at every level (origin, broadcast, track,
group). Producers write and consumers read; cloning a consumer shares the
subscription, and the last clone dropping closes it. Everything is
`async`, executor-agnostic, and errors are typed enums with `thiserror`.

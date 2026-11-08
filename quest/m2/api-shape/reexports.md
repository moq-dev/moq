# [L] Crates re-export no whole crate

## Goal

moq's crates re-export no whole crate, moq's own included, so a breaking
release of a dependency is no longer a semver break of the crates that
re-export it:

- hang: `pub use moq_net;` (`rs/hang/src/lib.rs`)
- moq-net: `pub use bytes;` and `pub use kio;` (`rs/moq-net/src/lib.rs`)
- moq-quic: `pub use rustls;` (`rs/moq-quic/src/lib.rs`)
- moq-rtmp: `pub use rustls;` (`rs/moq-rtmp/src/lib.rs`)
- moq-tokio: `moq_net`, `rustls`, `notify`, `jni`, and
  `moq_congestion as congestion` (`rs/moq-tokio/src/lib.rs`),
  `cli::usage_rs` (`rs/moq-tokio/src/cli.rs`), `jemalloc::tikv_jemallocator`
  (`rs/moq-tokio/src/jemalloc.rs`), and `quic::web`'s `http` and
  `web_transport_proto as proto` (`rs/moq-tokio/src/quic/web/mod.rs`)
- moq-uring: `url` (`rs/moq-uring/src/lib.rs`), and `quic::web`'s `http` and
  `web_transport_proto as proto` (`rs/moq-uring/src/quic/web.rs`)
- moq-hls: `axum` (`rs/moq-hls/src/lib.rs`)
- moq-webrtc: `axum` and `url` (`rs/moq-webrtc/src/lib.rs`)
- moq-archive: `object_store` (`rs/moq-archive/src/lib.rs`)
- moq-video: `ndk`, `objc2_core_foundation`, `objc2_core_video`, and
  `windows` (`rs/moq-video/src/lib.rs`), and `render::wgpu`
  (`rs/moq-video/src/render/mod.rs`)
- moq-mux: `container::fmp4::mp4_atom`, unless
  [media](/quest/m2/api-shape/media.md) has already dropped it, as
  [moq-dev/moq#8214](https://github.com/moq-dev/moq/pull/8214) plans
- qmux: `ws`'s `tokio_tungstenite` and `tungstenite`, unless
  [transport](/quest/m2/api-shape/transport.md) has already dropped them, as
  #8214 plans

No new one lands either:
[moq-quic-unused-surface](/quest/m2/moq-quic-unused-surface.md) names the
moq-quic items moq-tokio, moq-uring, and qmux need instead of adding
`pub use moq_quic;`.

Where a signature names a third-party type only in passing, moq wraps it,
so the dependency leaves moq's public API altogether: `reject` takes moq's
own status instead of `http::StatusCode` (moq-tokio's `quic::web` server
and connect requests, moq-uring's), and `watch::Files::new` returns
moq-tokio's error instead of `notify::Result`. A dependency moq's
signatures are built on stays in them and its users name it as their own
dependency: moq-net's types throughout hang and moq-tokio, `bytes::Bytes`
throughout moq-net, kio's waiters and clock, and the rustls configs
moq-quic, moq-rtmp, and moq-tokio take and return.

`moq_mux::container::producer::Guard`, which `container::Producer::modify`
returns, becomes nameable: `container` re-exports it beside `Producer`.

Every caller in this repository (about 110 files naming paths such as
`hang::moq_net`, `moq_tokio::moq_net`, `moq_net::kio`, and
`moq_tokio::rustls`, in apps, bindings, and tests) names the crate
directly instead. The PR's Impact section says how to migrate: add the
dependency at the version moq's `Cargo.lock` uses.

## Plan

Decided 2026-11-07 (maintainer, rust2's
[export census](https://github.com/moq-dev/rust2/pull/655)): ✅ drop
whole-crate re-exports from moq, wrapping the few third-party types its
signatures need. A whole-crate re-export makes that crate's whole API part
of moq's, so any breaking release of it breaks moq's semver. rust2's
exporter refuses each one with a span at its `pub use` (rejected there:
skipping them silently, as rustdoc does; exporting their whole APIs).

Decided 2026-11-07 (maintainer,
[moq-dev/moq#8233](https://github.com/moq-dev/moq/pull/8233)): ✅ every
whole-crate re-export goes, moq's own crates included, and
moq-quic-unused-surface adds no `pub use moq_quic;`. The semver reason
holds for any crate, and one rule needs no list. Rejected: only the ten
the census decision first listed; only third-party crates. Either would
need an exemption list in rust2's exporter, which is config.

Each re-export's own comment says why it exists, usually that a signature
names the crate (`usage_rs` for `cli::Merge` and `answer`, `kio` for the
`poll_*` waiters, `tikv_jemallocator` for a caller's `#[global_allocator]`,
`tokio_tungstenite` for qmux's `Client::with_connector`). Those reasons
justify the dependency in the signature, not the re-export: a caller who
names the type adds the crate itself. Wrap a type only where that removes
the crate from moq's public API; a wrapper that mirrors most of a crate's
API (rustls's configs, `Bytes`, moq-net's session types) is not worth its
weight. Check `cli::Merge`, `answer`, moq-hls's and moq-webrtc's axum
routers, `url::Url`, moq-archive's object store, moq-video's platform
handles, and `jni` the same way, since some may be wrappable.

`moq_quic::Side` is the other type rust2's census found unnameable, through
moq-tokio's `quic::driver` list naming `stream::Id` but not `Side`. Its
only uses there, `stream::Id::{new, initiator}`, go in
[moq-quic-unused-surface](/quest/m2/moq-quic-unused-surface.md), so this
quest leaves it.

rust2's [whole-crate re-export wait](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export/refusals/moq-reexports.md)
and [unreachable types wait](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export/refusals/moq-unreachable.md)
bump rust2's moq pin once this lands.

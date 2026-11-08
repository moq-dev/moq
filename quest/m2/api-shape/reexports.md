# [M] Crates re-export no whole crate

## Goal

moq's crates stop re-exporting whole crates, so a bump of a dependency is no
longer a semver break of moq's own crates:

- moq-net: `pub use bytes;` and `pub use kio;` (`rs/moq-net/src/lib.rs`)
- moq-quic: `pub use rustls;` (`rs/moq-quic/src/lib.rs`)
- moq-rtmp: `pub use rustls;` (`rs/moq-rtmp/src/lib.rs`)
- moq-tokio: `pub use rustls;` and `pub use notify;` (`rs/moq-tokio/src/lib.rs`),
  `cli::usage_rs` (`rs/moq-tokio/src/cli.rs`), and `quic::web::http`
  (`rs/moq-tokio/src/quic/web/mod.rs`)
- moq-uring: `quic::web::http` (`rs/moq-uring/src/quic/web.rs`)
- moq-mux: `container::fmp4::mp4_atom`, unless
  [media](/quest/m2/api-shape/media.md) has already dropped it, as
  [moq-dev/moq#8214](https://github.com/moq-dev/moq/pull/8214) plans

Where a signature names a third-party type only in passing, moq wraps it,
so the dependency leaves moq's public API altogether: `reject` takes moq's
own status instead of `http::StatusCode` (moq-tokio's `quic::web` server
and connect requests, moq-uring's), and `watch::Files::new` returns
moq-tokio's error instead of `notify::Result`. A dependency moq's
signatures are built on stays in them and its users name it as their own
dependency: `bytes::Bytes` throughout moq-net, kio's waiters and clock, and
the rustls configs moq-quic, moq-rtmp, and moq-tokio take and return.

`moq_mux::container::producer::Guard`, which `container::Producer::modify`
returns, becomes nameable: `container` re-exports it beside `Producer`.

Every caller in this repository (about 70 paths such as `moq_net::bytes`,
`moq_tokio::rustls`, and `moq_net::kio`, in apps, bindings, and tests)
names the crate directly instead. The PR's Impact section says how to
migrate: add the dependency at the version moq's `Cargo.lock` uses.

## Plan

Decided 2026-11-07 (maintainer, rust2's
[export census](https://github.com/moq-dev/rust2/pull/655)): ✅ drop
whole-crate re-exports from moq, wrapping the few third-party types its
signatures need. A whole-crate re-export makes that crate's whole API part
of moq's, so any bump of it breaks moq's semver. rust2's exporter refuses
each one with a span at its `pub use` (rejected there: skipping them
silently, as rustdoc does; exporting their whole APIs).

Each re-export's own comment says why it exists, usually that a signature
names the crate (`usage_rs` for `cli::Merge` and `answer`, `kio` for the
`poll_*` waiters). Those reasons justify the dependency in the signature,
not the re-export: a caller who names the type adds the crate itself.
Wrap a type only where that removes the crate from moq's public API; a
wrapper that mirrors most of a crate's API (rustls's configs, `Bytes`) is
not worth its weight. Check `cli::Merge` and `answer` the same way, since
usage-rs may be wrappable too.

`moq_quic::Side` is the other type rust2's census found unnameable, through
moq-tokio's `quic::driver` list naming `stream::Id` but not `Side`. Its
only uses there, `stream::Id::{new, initiator}`, go in
[moq-quic-unused-surface](/quest/m2/moq-quic-unused-surface.md), so this
quest leaves it.

rust2's [whole-crate re-export wait](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export/refusals/moq-reexports.md)
and [unreachable types wait](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export/refusals/moq-unreachable.md)
bump rust2's moq pin once this lands.

## Waiting

- Maintainer: decide whether this quest also drops the whole-crate
  re-exports the census decision did not list. rust2's exporter refuses
  every one in the crates it exports by its form, so hang's and moq-tokio's
  `pub use moq_net;`, moq-tokio's `pub use moq_congestion as congestion;`,
  and `quic::web`'s `pub use web_transport_proto as proto;` (moq-tokio and
  moq-uring) keep rust2's census from emptying while they stay. The same
  reason covers moq-hls's and moq-webrtc's `axum`, moq-webrtc's and
  moq-uring's `url`, moq-archive's `object_store`, moq-video's `ndk`,
  `objc2_core_foundation`, `objc2_core_video`, `windows`, and `wgpu`,
  moq-tokio's `jni` and `tikv_jemallocator`, and the `pub use moq_quic;`
  that [moq-quic-unused-surface](/quest/m2/moq-quic-unused-surface.md)
  plans in moq-tokio, moq-uring, and qmux. qmux's `tokio_tungstenite` is
  [transport](/quest/m2/api-shape/transport.md)'s, as #8214 plans.
  Options: drop every whole-crate re-export, and moq-quic-unused-surface
  drops its moq-quic name lists without re-exporting the crate (recommended,
  since the reason does not depend on which crate it is and one rule needs
  no list); drop only the listed ten. Check: this section is gone and the
  Goal lists the answer.

# [M] An async QUIC driver without moq-net

## Goal

An application runs `moq-quic` on tokio without depending on moq-net:
`moq_tokio::quic::driver`'s endpoint, connection, and stream futures, with
send groups, `reset_at`, receive leases, and live `Limits`, become documented
public API that builds without moq-net. Any Rust MoQ implementation, or any
QUIC application, can embed the stack moq-dev ships, instead of forking
quinn's async layer again.

`cargo tree -e normal` on a driver-only build of moq-tokio
(`--no-default-features`, the driver feature, and a crypto provider) shows no
moq-net, kio, reqwest, or notify, and no crate from quinn, noq, or
moq-dev/web-transport. CI builds that feature set, and the driver's tests stay
in the merge gate.

## Plan

Where it stands:

- The driver is quinn's async layer on moq-quic, kept as moq-tokio modules,
  tokio-only (decided 2026-10-06 in the fork epic, over `moq-quic-udp` and
  `moq-quic-tokio` crates). The udp half is now `moq_sock::udp`.
- Its only moq-net type is `moq_net::transport::Wire`, a pair of byte
  counters (`quic/driver/connection.rs`). `quic/backend.rs` also takes
  `moq_net::Versions` for ALPN lists, but sits above the driver.
- moq-net is a required dependency of moq-tokio, even without the `quic`
  feature that gates the driver, so a consumer cannot take the driver alone.
- The driver is public but `#[doc(hidden)]`, and is not on `release`.

Decided 2026-10-20 (maintainer): `moq-quic` powers qmux and WebTransport,
and moq takes no new work in, or dependency on, moq-dev/web-transport, noq
or quinn. The driver is that stack's tokio runtime, beside moq-uring's
driver and the planned in-tree qmux, so this quest moves no code out of the
tree. quinn stays the upstream the fork cherry-picks from, not a dependency.

Decided 2026-10-20 (maintainer): the driver stays in moq-tokio, at
`moq_tokio::quic::driver`, and moq-net becomes optional there. No new crate
and no feature on moq-quic: the driver keeps its home and its tests, and the
sans-IO core that moq-uring and qmux share gets no runtime. It stays a
`driver` submodule, since `quic::Connection` and `quic::Error` already name
the MoQ backend's types.

Rejected: a `moq-quic-tokio` crate (a new published crate and a reversal of
the 2026-10-06 decision) and a `tokio` feature on moq-quic (a runtime in the
sans-IO core, and the driver's tests behind a non-default gate).

The work:

- The first commit replaces `Wire` with counters owned by the driver.
  moq-tokio converts at its boundary, and moq-net's wire stats do not change.
- moq-net moves behind a default feature, which every transport feature also
  enables, since none works without it. `web-transport-trait` moves with it:
  every user (`transport`, `server`, `tcp`, `quic::web`) is on the moq-net
  side. Every moq-net use in moq-tokio, including the `moq_net` re-export,
  gets the gate.
- A driver-only feature turns on `quic::driver` alone, and `quic` enables it
  plus `quic::backend` and `quic::web`. `quic`'s other dependencies (reqwest,
  notify through `watch`, kio, http, web-transport-proto,
  rustls-native-certs, dns-lookup) stay on `quic`, so a driver-only build
  drops them. Name both features in the implementing PR.
- Out of scope: moq-tokio's unconditional dependencies, which a driver-only
  build still pulls through its `cli`, `tls`, and `settings` modules: tokio
  `full`, rustls and rustls-platform-verifier, x509-parser, serde, toml,
  tracing-subscriber, and usage-rs, plus rcgen and rustls-webpki from the
  crypto-provider feature. None is MoQ code; splitting them is its own quest
  if an embedder needs it.
- Drop the driver's `#[doc(hidden)]` and its "not a stable API" comment, and
  document every exported item. The driver keeps quinn's rustfmt config.
- In-tree consumers with `default-features = false` and no transport feature
  that use moq-net-backed modules (moq-room, moq-boy, moq-bench) enable the
  new moq-net feature.
- `quic::web` and `quic::backend` keep their features and dependencies here.
  [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md) owns their
  API changes, such as `quic::web::Session` dropping its `Deref`, and proceeds
  on top. Driver API changes that quest makes, such as aligning the stream
  setters with moq-uring's, land in the driver here; neither quest waits on
  the other.
- moq-uring's driver is out of scope. It is `!Send`, and coupled to moq-net
  through `server::Activity` as well as `Wire`.

Decided 2026-10-20: m1, because making moq-net optional is a breaking change.
A build with `default-features = false` and no transport feature loses
`client`, `connect`, `connection`, `listen`, `origin`, `transport`, the root
re-exports built on them (`Client`, `Connection`, `Error`, ...), and the
`moq_net` re-export, so it lands before the cut. Add an entry under
`## Unreleased` in `rs/moq-tokio/MIGRATE.md`: enable the new moq-net feature
or a transport feature to keep them. Public API: the two new features, and
`quic::driver` becomes supported. Wire: none.

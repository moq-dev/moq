# [S] moq-tokio: no dead code without a transport

## Goal

`cargo clippy -p moq-tokio --no-default-features -- -D warnings` passes, and so
does each single-feature build, so the configurations `just rs tokio-features`
compiles are held to the same bar as the default one.

## Plan

The backend-less build compiles with 11 warnings, every one an item whose only
callers sit behind a transport gate:

- unused imports `Member` and `Shard` (rs/moq-tokio/src/listen.rs:150)
- `Client::timeout` never read (client.rs:75)
- `listen::Config::validate` (listen.rs:305)
- `quic::Config::validate` (quic.rs:450), `MAX_IDLE_TIMEOUT` (:488),
  `validate_idle_timeout` (:494), `MAX_VARINT` (:503), `validate_windows`
  (:511)
- `tls::Peers::contains_raw` (tls.rs:395), `tls::CustomRoots::load` (:657),
  `tls::Certificates::empty` (:1602)

Gating each by hand would spell the six-way "has a transport" `any(...)` in
several more places, which is what makes this its own change rather than a
follow-on edit. Give the crate a private `_transport` feature, the way `_certs`
already covers the serving side, enabled from `noq`, `quinn`, `quiche`, `iroh`,
`websocket`, and `tcp`. Collapse onto it only the gates that spell exactly that
set: `lib.rs:57` and `:91`, and `client.rs:110` and `:126`. The narrower gates
stay as they are, because they mean something else: `client.rs:63` excludes
`iroh` on purpose, and `server.rs:21` excludes `tcp`/`uds`. `server::Parts`
already carries targeted `expect(dead_code)` attributes (server.rs:120-134)
and needs nothing.

Then swap `cargo check` for `clippy -- -D warnings` in `just rs tokio-features`
(rs/justfile:401-428) so the bar holds once it is met. Leave the loop at :419
alone: it deliberately expects `quinn`/`noq` without a crypto provider to fail
the build, and asserts on the error text.

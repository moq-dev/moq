# [S] moq-tokio: no dead code without a transport

## Goal

`cargo clippy -p moq-tokio --no-default-features -- -D warnings` passes, and so
does each single-feature build, so the configurations `just rs features` compiles
per crate are held to the same bar as the default one.

## Plan

The backend-less build compiles, but emits dead-code warnings, every one of them
an item whose only callers sit behind a transport gate: `listen::Shard`,
`listen::Config::validate`, `quic::Config::validate` with `MAX_IDLE_TIMEOUT` and
`validate_idle_timeout`, `Client`'s `timeout` field, `server::Parts::Shard`,
`tls::Peers::contains_raw`, `tls::CustomRoots::load`, and
`tls::Certificates::empty`.

Gating each by hand would spell the six-way "has a transport" `any(...)` in
several more places, which is what makes this its own change rather than a
follow-on edit. Give the crate a private `_transport` feature, the way `_certs`
already covers the serving side, enabled from `noq`, `quinn`, `quiche`, `iroh`,
`websocket`, and `tcp`. Collapse the long `any(...)` gates already in `lib.rs`,
`client.rs`, and `server.rs` onto it too, so the crate has one spelling of the
idea rather than two.

Then swap `cargo check` for `clippy -- -D warnings` in the per-crate passes of
`just rs features`, so the bar holds once it is met.

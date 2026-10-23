# [S] Unpublish what nothing uses

## Goal

Packages, crates, features, and listeners with no consumer stop shipping in
this release or stop being on by default, so the release promises nothing
it does not use.

## Plan

Decided 2026-10-22 (API audit):

- `@moq/p2p` (`js/p2p/package.json`, first publish at 0.1.0, only `test/p2p`
  consumes it): `"private": true` until [P2P watch](/quest/m2/p2p/watch.md).
- `moq-e2ee`: `publish = false` until [E2EE CLI](/quest/m2/e2ee/cli.md);
  `@moq/e2ee` is already private.
- Rust `moq-room`: `publish = false`; no consumer here or in moq.pro (the JS
  package has `demo/web`). `@moq/room` stops re-exporting the whole Hang,
  Net, Publish, Signals, and Watch namespaces.
- `moq-boy`: `[[bin]] required-features = ["openh264"]`, so `cargo install`
  fails at build time instead of building a binary that refuses to start.
- moq-cli: drop `webrtc` from default features
  (`rs/moq-cli/Cargo.toml`); the CLI never calls it, yet every build pulls
  str0m, moq-ice, mdns-sd, and if-addrs. moq-tokio's `webrtc` module stays
  behind its feature, `#[doc(hidden)]` until [P2P CLI](/quest/m2/p2p/cli.md).
- moq-relay's STUN server (`pub mod stun`, `Config.stun`, 537 lines): gate
  behind a non-default `stun` feature until
  [UDP demux](/quest/m2/p2p/udp-demux.md); nothing sets `stun.bind` and it
  forces `stun-types` onto default-features-off embedders.
- moq-rtc: revert `Client::publish`/`subscribe` to `Result<()>` and drop
  `pub mod wire` and `Response.wire` until
  [wire stats](/quest/m2/wire-stats/README.md) lands its consumer.
- moq-ice stays published (moq-tokio and moq-rtc depend on it); document it
  as internal with no stability promise and give `Socket` a constructor that
  enforces non-empty `locals`.
- `qmux` 0.7.0 publishes without a README; add one.

Public API: removes unconsumed surface. Wire: none.

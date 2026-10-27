# [XS] ACME on graviola

## Goal

An upstream ACME crate takes its crypto from something other than ring or
aws-lc-rs, so [moq-relay on graviola](/quest/m3/pure-crypto/relay-graviola.md)
keeps the default `acme` feature: a rustls-acme release with a provider hook,
or an instant-acme 0.9 release, whichever comes first. Check both on crates.io.
This quest only waits; delete it once one holds.

## Plan

Upstream status on 2026-10-25:

- rustls-acme 0.15.4 hits a `compile_error!` without ring or aws-lc-rs.
  [FlorianUekermann/rustls-acme#62](https://github.com/FlorianUekermann/rustls-acme/issues/62)
  tracks a hook, with no work since 2024. A rustls `CryptoProvider` alone is
  not enough: rustls-acme also generates account and certificate keys and
  hashes key authorizations.
- instant-acme's `main` takes its crypto through its own `CryptoProvider`
  traits ([djc/instant-acme#268](https://github.com/djc/instant-acme/pull/268)),
  for 0.9.0; 0.8.5 still requires ring or aws-lc-rs. instant-acme is only the
  ACME client, so moq-relay would own the renewal loop and the TLS-ALPN-01
  resolver that rustls-acme provides today.

Decided by the maintainer on 2026-10-26: moq waits passively and posts nothing
upstream, since the relay switch is deferred.

Public API: none. Wire: none.

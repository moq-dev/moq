# [S] ACME on graviola

## Goal

moq-relay's graviola builds turn ACME back on once an upstream ACME crate
takes its crypto from something other than ring or aws-lc-rs: a rustls-acme
release with a provider hook, or instant-acme 0.9, whichever comes first.
Check the upstream status below on crates.io and GitHub.

## Plan

Decided by the maintainer on 2026-10-25: ACME has no released user yet, so
[moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md) builds without
it, and moq carries no patched rustls-acme. This quest waits for upstream.

Upstream status on 2026-10-25:

- rustls-acme 0.15.4 hits a `compile_error!` without ring or aws-lc-rs. Its
  maintainer filed
  [FlorianUekermann/rustls-acme#62](https://github.com/FlorianUekermann/rustls-acme/issues/62)
  in 2024, with no work since. A rustls `CryptoProvider` alone is not enough:
  rustls-acme also generates account and certificate keys and hashes key
  authorizations.
- instant-acme's `main` already takes its crypto through its own
  `CryptoProvider` traits (signing keys, HMAC, SHA-256), merged in
  [djc/instant-acme#268](https://github.com/djc/instant-acme/pull/268), as
  0.9.0. Its latest release, 0.8.5, still requires ring or aws-lc-rs.
  instant-acme is only the ACME client: moq-relay would own the renewal loop
  and the TLS-ALPN-01 resolver that rustls-acme provides today.

Optional: propose a hook on rustls-acme#62 shaped like instant-acme's traits.
Decided by the maintainer on 2026-10-25: posting upstream for this quest is
approved, so whoever starts it may post without asking.

Public API: moq-relay's `acme` feature works with graviola again. Wire: none.

## Related

- [moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md) - builds without ACME until this lands

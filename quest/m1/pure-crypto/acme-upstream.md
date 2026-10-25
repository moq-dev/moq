# [M] Provider-neutral ACME

## Goal

moq-relay's ACME runs on graviola through a released upstream crate that
neither requires ring nor aws-lc-rs, and moq deletes its patched rustls-acme
copy. Check the upstream status below on crates.io and GitHub.

## Plan

Decided with the maintainer on 2026-10-24, from
[kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13)'s decision 1:
upstream a provider-neutral crypto hook, and carry the spike's patched copy
(crypto on graviola and P-256) only until it lands.

Upstream status on 2026-10-25:

- rustls-acme 0.15.4 hits a `compile_error!` without ring or aws-lc-rs. Its
  maintainer filed
  [FlorianUekermann/rustls-acme#62](https://github.com/FlorianUekermann/rustls-acme/issues/62)
  in 2024, with no work since. A rustls `CryptoProvider` alone is not enough:
  rustls-acme also generates account and certificate keys and hashes key
  authorizations. Outside PRs there wait months
  ([#102](https://github.com/FlorianUekermann/rustls-acme/pull/102),
  [#91](https://github.com/FlorianUekermann/rustls-acme/pull/91)).
- instant-acme's `main` already takes its crypto through its own
  `CryptoProvider` traits (signing keys, HMAC, SHA-256), merged in
  [djc/instant-acme#268](https://github.com/djc/instant-acme/pull/268), as
  0.9.0. Its latest release, 0.8.5, still requires ring or aws-lc-rs.
  instant-acme is only the ACME client: moq-relay would own the renewal loop
  and the TLS-ALPN-01 resolver that rustls-acme provides today.

Open (recommendation: the first, then the second if it stalls):

1. Propose on rustls-acme#62 a hook shaped like instant-acme's traits, and
   carry the patched copy until a release ships it. Least moq code, but it
   depends on a slow upstream.
2. Once instant-acme 0.9 releases, move moq-relay onto it with a graviola
   provider and drop rustls-acme, which also drops the rcgen 0.13 and
   async-web-client duplicates the
   [dependency audit](/quest/m2/audit-deps.md) lists. No upstream code, but
   moq-relay owns more of the ACME flow.

Decided by the maintainer on 2026-10-25: posting upstream for this quest is
approved, so whoever starts it may post without asking.

Public API: none. Wire: none.

## Related

- [moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md) - carries the patched copy until this lands

# [S] Provider-neutral rustls-acme upstream

## Goal

rustls-acme takes its crypto from any rustls `CryptoProvider` instead of
hard-requiring ring or aws-lc-rs, in a release moq can depend on, and moq drops
its patched copy. Check: rustls-acme's latest release builds with neither its
`ring` nor its `aws-lc-rs` feature.

## Plan

Decided with the maintainer on 2026-10-24, from
[kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13)'s decision 1:
upstream a provider-neutral crypto hook, as rustls-acme's own TODO asks, and
carry the spike's patched copy (crypto on graviola and P-256) only until it
lands. instant-acme hard-requires ring or aws-lc-rs too.

Posting upstream is outside the kixelated and moq-dev organizations, so it needs
the maintainer's approval when this starts.

Public API: none. Wire: none.

## Related

- [moq-relay on graviola](/quest/m2/pure-crypto/relay-graviola.md) - carries the patched copy until this lands
- [ACME renewal budget](/quest/m1/acme-renewal-budget.md) - also works around rustls-acme's order loop
